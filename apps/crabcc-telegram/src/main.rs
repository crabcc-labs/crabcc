//! CrabCC_bot — Telegram interface for crabcc agents.
//!
//! Issue #134. Uses teloxide (https://github.com/teloxide/teloxide).
//!
//! # Single-owner lockdown
//! The bot is hard-locked to a single Telegram user ID — see
//! `OWNER_TELEGRAM_USER_ID` below. Every other sender is silently dropped
//! after a one-line "Not authorised" reply. The previous env-driven
//! `ALLOWED_TELEGRAM_IDS` allowlist (with its "empty == open to all"
//! footgun default) has been removed; widening access requires a source
//! change + recompile.
//!
//! # Required env vars
//!   TELEGRAM_BOT_TOKEN   — from BotFather (never commit this)
//!
//! # Optional env vars
//!   CRABCC_SERVE_URL     — crabcc serve base URL (default http://localhost:8090)
//!   CRABCC_PUBLIC_URL    — public HTTPS URL for /live Mini App
//!                          Use `ngrok http 8090` or `cloudflared tunnel --url http://localhost:8090`
//!                          Mini App docs: https://core.telegram.org/bots/webapps
//!                          Init data validation: https://github.com/escwxyz/init-data-rs
//!
//! # Mobile-network resilience
//! Backend HTTP calls (bot → `crabcc serve`) go through a shared reqwest
//! client with explicit `connect_timeout` + `timeout`, plus a 3-attempt
//! exponential-backoff retry on connect/timeout/5xx. Subprocess invocations
//! of the `crabcc` CLI are bounded by `SUBPROCESS_TIMEOUT`. Telegram-side
//! reconnects (long-poll) are handled by teloxide's dispatcher.
//!
//! # Run
//!   TELEGRAM_BOT_TOKEN=<token> cargo run -p crabcc-telegram
//!   # or via Taskfile:
//!   task telegram-bot

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::Url;
use teloxide::{
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, ParseMode, WebAppInfo},
    utils::command::BotCommands,
};
use tokio::process::Command as TokioCommand;

// ── owner lockdown ────────────────────────────────────────────────────────────

/// The one and only Telegram user this bot will respond to (@g0ph3r).
/// Compile-time hardcoded — env can't widen this. To change: edit this
/// const and rebuild. To grant access to additional accounts: don't —
/// stand up a second bot pointed at the same `crabcc serve` instead.
const OWNER_TELEGRAM_USER_ID: u64 = 5_875_395_828;

/// Hard ceiling on any `crabcc` subprocess invocation. Prevents a stuck
/// child from wedging the bot's command handler indefinitely.
const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(120);

/// HTTP client config — keeps a single backend hang from killing UX.
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP_RETRY_ATTEMPTS: usize = 3;
const HTTP_RETRY_BASE_DELAY: Duration = Duration::from_millis(300);

fn is_owner(user_id: u64) -> bool {
    user_id == OWNER_TELEGRAM_USER_ID
}

// ── env config ────────────────────────────────────────────────────────────────

struct Config {
    serve_url: Url,
    public_url: Option<Url>,
    /// Pre-built `{serve_url}/live`. Avoids runtime parse panics in handlers.
    serve_live_url: Url,
    /// Pre-built `{public_url}/live` if `public_url` is set.
    public_live_url: Option<Url>,
    /// Pre-built `{serve_url}/api/agents`.
    agents_api_url: Url,
    /// Shared HTTP client built once with `connect_timeout` + `timeout`.
    /// Cloning is cheap (`Arc` internally). Mobile-network resilience knob.
    http: reqwest::Client,
}

impl Config {
    fn from_env() -> Result<Self> {
        let default_serve =
            Url::parse("http://localhost:8090/").expect("default serve URL is valid");

        let serve_url = match std::env::var("CRABCC_SERVE_URL") {
            Ok(raw) => Url::parse(&raw).unwrap_or_else(|e| {
                tracing::warn!(
                    env = "CRABCC_SERVE_URL",
                    value = %raw,
                    error = %e,
                    "invalid serve URL, falling back to http://localhost:8090/"
                );
                default_serve.clone()
            }),
            Err(_) => default_serve.clone(),
        };

        let public_url = std::env::var("CRABCC_PUBLIC_URL")
            .ok()
            .and_then(|raw| match Url::parse(&raw) {
                Ok(u) => Some(u),
                Err(e) => {
                    tracing::warn!(
                        env = "CRABCC_PUBLIC_URL",
                        value = %raw,
                        error = %e,
                        "invalid public URL, ignoring"
                    );
                    None
                }
            });

        // join() resolves "live" / "api/agents" against the base URL using
        // proper URL semantics (handles trailing slash). Cannot fail for an
        // already-valid http(s) base.
        let serve_live_url = serve_url
            .join("live")
            .expect("joining 'live' to a valid http(s) base URL is infallible");
        let public_live_url = public_url.as_ref().map(|u| {
            u.join("live")
                .expect("joining 'live' to a valid http(s) base URL is infallible")
        });
        let agents_api_url = serve_url
            .join("api/agents")
            .expect("joining 'api/agents' to a valid http(s) base URL is infallible");

        let http = reqwest::Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_REQUEST_TIMEOUT)
            .user_agent(concat!("crabcc-telegram/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("build reqwest client")?;

        Ok(Self {
            serve_url,
            public_url,
            serve_live_url,
            public_live_url,
            agents_api_url,
            http,
        })
    }
}

// ── bot commands ──────────────────────────────────────────────────────────────

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "CrabCC commands:")]
enum Cmd {
    #[command(description = "Show this help")]
    Help,
    #[command(description = "Run a crabcc agent — /agent <task>")]
    Agent(String),
    #[command(description = "List recent agent runs")]
    Status,
    #[command(description = "Run doctor checks")]
    Doctor,
    #[command(description = "Search memory — /search <query>")]
    Search(String),
    #[command(description = "Open live dashboard")]
    Dashboard,
    #[command(description = "Kill a running agent — /kill <id>")]
    Kill(String),
    #[command(description = "Index the current repo")]
    Index,
}

// ── helpers ───────────────────────────────────────────────────────────────────

async fn crabcc(args: &[&str]) -> Result<String> {
    let mut cmd = TokioCommand::new("crabcc");
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Detach the child from the bot's controlling terminal so a Ctrl+C
        // on the bot doesn't propagate. Important for the menubar+launchd
        // run path where there's no controlling tty anyway.
        .kill_on_drop(true);
    let child = cmd.spawn().context("spawn crabcc")?;

    // Hard timeout — prevents a stuck child (e.g. `crabcc index` on a
    // very large repo, or a network-blocked subcommand) from wedging
    // the bot's command handler indefinitely.
    let out = match tokio::time::timeout(SUBPROCESS_TIMEOUT, child.wait_with_output()).await {
        Ok(r) => r.context("wait")?,
        Err(_) => {
            tracing::warn!(
                ?args,
                timeout_secs = SUBPROCESS_TIMEOUT.as_secs(),
                "crabcc subprocess timeout"
            );
            return Ok(format!(
                "⌛ subprocess timed out after {}s — try a narrower request or check `crabcc doctor`",
                SUBPROCESS_TIMEOUT.as_secs()
            ));
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if out.status.success() {
        Ok(if stdout.is_empty() {
            "Done.".into()
        } else {
            stdout
        })
    } else {
        Ok(format!(
            "error:\n{}",
            if stderr.is_empty() { stdout } else { stderr }
        ))
    }
}

/// Fetch JSON with retry + exponential backoff. Mobile-network resilience:
/// `connect_timeout` + `timeout` come from the shared `reqwest::Client`,
/// `HTTP_RETRY_ATTEMPTS` retries absorb transient blips before surfacing.
async fn fetch_json(http: &reqwest::Client, url: &str) -> Result<serde_json::Value> {
    let mut delay = HTTP_RETRY_BASE_DELAY;
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=HTTP_RETRY_ATTEMPTS {
        match http.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return resp.json().await.context("decode JSON");
                }
                let snippet = resp.text().await.unwrap_or_default();
                let snippet = snippet.chars().take(200).collect::<String>();
                last_err = Some(anyhow!("HTTP {status}: {snippet}"));
                if !status.is_server_error() {
                    // 4xx — won't fix itself; bail without retry.
                    return Err(last_err.unwrap());
                }
            }
            Err(e) => {
                let transient = e.is_timeout() || e.is_connect() || e.is_request();
                tracing::warn!(
                    attempt,
                    transient,
                    error = %e,
                    "fetch_json retry"
                );
                last_err = Some(e.into());
                if !transient {
                    return Err(last_err.unwrap());
                }
            }
        }
        if attempt < HTTP_RETRY_ATTEMPTS {
            tokio::time::sleep(delay).await;
            delay = delay.saturating_mul(2);
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("retry exhausted")))
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ── command handlers ──────────────────────────────────────────────────────────

async fn handle(
    bot: Bot,
    msg: Message,
    cmd: Cmd,
    cfg: Arc<Config>,
) -> ResponseResult<()> {
    let user_id = msg.from().map(|u| u.id.0).unwrap_or(0);
    if !is_owner(user_id) {
        tracing::warn!(user_id, "rejected non-owner message");
        bot.send_message(msg.chat.id, "⛔ Not authorised.").await?;
        return Ok(());
    }

    match cmd {
        Cmd::Help => {
            bot.send_message(msg.chat.id, Cmd::descriptions().to_string())
                .await?;
        }

        Cmd::Agent(task) => {
            if task.trim().is_empty() {
                bot.send_message(msg.chat.id, "Usage: /agent <task description>")
                    .await?;
                return Ok(());
            }
            let status = bot
                .send_message(msg.chat.id, format!("🦀 Starting agent: _{}_", &task))
                .parse_mode(ParseMode::MarkdownV2)
                .await?;
            // Spawn agent non-blocking (fire and forget) so Telegram doesn't time out.
            let task_clone = task.clone();
            tokio::spawn(async move {
                let _ = crabcc(&["agent", "--run", &task_clone, "--backend", "ollama"]).await;
            });
            bot.edit_message_text(
                msg.chat.id,
                status.id,
                format!("🦀 Agent launched: `{}`\nUse /status to check progress\\.", truncate(&task, 60)),
            )
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        }

        Cmd::Status => {
            let raw = crabcc(&["agent-ls", "--limit", "5"]).await.unwrap_or_default();
            let text = format!("*Recent agents*\n```\n{}\n```", truncate(&raw, 3000));
            bot.send_message(msg.chat.id, text)
                .parse_mode(ParseMode::MarkdownV2)
                .await?;
        }

        Cmd::Doctor => {
            let raw = crabcc(&["doctor"]).await.unwrap_or_default();
            let text = format!("*Doctor report*\n```\n{}\n```", truncate(&raw, 3000));
            bot.send_message(msg.chat.id, text)
                .parse_mode(ParseMode::MarkdownV2)
                .await?;
        }

        Cmd::Search(query) => {
            if query.trim().is_empty() {
                bot.send_message(msg.chat.id, "Usage: /search <query>").await?;
                return Ok(());
            }
            let raw = crabcc(&["memory", "search", &query, "--limit", "5"])
                .await
                .unwrap_or_default();
            let text = format!("*Memory search:* `{}`\n```\n{}\n```", query, truncate(&raw, 3000));
            bot.send_message(msg.chat.id, text)
                .parse_mode(ParseMode::MarkdownV2)
                .await?;
        }

        Cmd::Dashboard => {
            // Telegram Mini App requires HTTPS. Set CRABCC_PUBLIC_URL via:
            //   ngrok:        ngrok http 8090
            //   cloudflared:  cloudflared tunnel --url http://localhost:8090
            // Docs: https://core.telegram.org/bots/webapps
            let keyboard = if let Some(public_live) = cfg.public_live_url.clone() {
                InlineKeyboardMarkup::new([
                    [InlineKeyboardButton::web_app(
                        "📊 Open live dashboard",
                        WebAppInfo { url: public_live },
                    )],
                    [InlineKeyboardButton::url(
                        "🔗 Direct link",
                        cfg.serve_live_url.clone(),
                    )],
                ])
            } else {
                // No HTTPS — plain URL button + hint to set CRABCC_PUBLIC_URL
                InlineKeyboardMarkup::new([[InlineKeyboardButton::url(
                    "📊 Open live dashboard (local only)",
                    cfg.serve_live_url.clone(),
                )]])
            };

            // Also send a text snapshot of current agent state
            let snapshot = match fetch_json(&cfg.http, cfg.agents_api_url.as_str()).await {
                Ok(v) => {
                    let active: Vec<_> = v["agents"]
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .filter(|a| a["status"] == "running")
                        .map(|a| {
                            format!(
                                "• {} ({})",
                                a["name"].as_str().unwrap_or("?"),
                                a["id"].as_str().unwrap_or("?")
                            )
                        })
                        .collect();
                    if active.is_empty() {
                        "No active agents.".to_string()
                    } else {
                        format!("Active agents:\n{}", active.join("\n"))
                    }
                }
                Err(_) => "crabcc serve not reachable — start with `crabcc serve`.".into(),
            };

            bot.send_message(msg.chat.id, snapshot)
                .reply_markup(keyboard)
                .await?;
        }

        Cmd::Kill(id) => {
            if id.trim().is_empty() {
                bot.send_message(msg.chat.id, "Usage: /kill <agent-id>").await?;
                return Ok(());
            }
            let raw = crabcc(&["agent-kill", id.trim()]).await.unwrap_or_default();
            bot.send_message(msg.chat.id, format!("Kill result:\n{}", truncate(&raw, 500)))
                .await?;
        }

        Cmd::Index => {
            bot.send_message(msg.chat.id, "⚙️ Indexing…").await?;
            let raw = crabcc(&["index"]).await.unwrap_or_default();
            bot.send_message(msg.chat.id, format!("✓ Done:\n{}", truncate(&raw, 500)))
                .await?;
        }
    }

    Ok(())
}

// ── main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("crabcc_telegram=info".parse().unwrap()),
        )
        .init();

    // CLI subcommands — kept tiny to avoid pulling in clap. We only need
    // `install-service`, `uninstall-service`, and the implicit "run".
    let argv: Vec<String> = std::env::args().collect();
    match argv.get(1).map(|s| s.as_str()) {
        Some("install-service") => return service::install(),
        Some("uninstall-service") => return service::uninstall(),
        Some("status-service") => return service::status(),
        Some("--help" | "-h") => {
            println!(
                "crabcc-telegram — CrabCC_bot\n\
                 \n\
                 Usage:\n  \
                   crabcc-telegram                    # run the bot in foreground\n  \
                   crabcc-telegram install-service    # install LaunchAgent (macOS) and start\n  \
                   crabcc-telegram uninstall-service  # bootout + remove LaunchAgent\n  \
                   crabcc-telegram status-service     # show running state + last exit code\n\
                 \n\
                 Owner-locked to Telegram user id {OWNER_TELEGRAM_USER_ID}."
            );
            return Ok(());
        }
        _ => {}
    }

    let cfg = Arc::new(Config::from_env().context("config from env")?);

    tracing::info!(
        serve_url = %cfg.serve_url,
        has_public_url = cfg.public_url.is_some(),
        owner = OWNER_TELEGRAM_USER_ID,
        "CrabCC_bot starting"
    );

    let bot = Bot::from_env(); // reads TELEGRAM_BOT_TOKEN

    let handler = Update::filter_message()
        .filter_command::<Cmd>()
        .endpoint(move |bot: Bot, msg: Message, cmd: Cmd| {
            handle(bot, msg, cmd, Arc::clone(&cfg))
        });

    Dispatcher::builder(bot, handler)
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

// ── service install / uninstall (macOS LaunchAgent first; Linux TBD) ─────────

mod service {
    use anyhow::{anyhow, Context, Result};
    use std::path::PathBuf;

    const LABEL: &str = "com.crabcc.telegram-bot";

    #[cfg(target_os = "macos")]
    pub fn install() -> Result<()> {
        let plist_path = la_path()?;
        let exe = std::env::current_exe().context("current_exe")?;
        let logs_dir = home()?.join("Library/Logs/Crabcc");
        std::fs::create_dir_all(&logs_dir).context("create logs dir")?;

        // KeepAlive=true + RunAtLoad=true => launchd restarts on crash AND
        // on login. ThrottleInterval guards a tight crash loop. We don't
        // pin the working directory — TELEGRAM_BOT_TOKEN comes from the
        // user's launchd env (set via `launchctl setenv` or .env loaded
        // by the bot itself before this lands).
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>           <string>{label}</string>
  <key>ProgramArguments</key>
    <array><string>{exe}</string></array>
  <key>RunAtLoad</key>       <true/>
  <key>KeepAlive</key>       <true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key>     <string>Interactive</string>
  <key>StandardOutPath</key> <string>{logs}/telegram-bot.out.log</string>
  <key>StandardErrorPath</key><string>{logs}/telegram-bot.err.log</string>
  <key>EnvironmentVariables</key>
    <dict>
      <key>RUST_LOG</key><string>crabcc_telegram=info,info</string>
    </dict>
</dict>
</plist>
"#,
            label = LABEL,
            exe = exe.display(),
            logs = logs_dir.display(),
        );
        std::fs::write(&plist_path, plist).with_context(|| format!("write {}", plist_path.display()))?;

        // bootout existing first so an updated plist is honored.
        let uid = unsafe { libc_getuid() };
        let _ = std::process::Command::new("/bin/launchctl")
            .args(["bootout", &format!("gui/{uid}/{LABEL}")])
            .status();
        let st = std::process::Command::new("/bin/launchctl")
            .args(["bootstrap", &format!("gui/{uid}"), plist_path.to_str().unwrap()])
            .status()
            .context("launchctl bootstrap")?;
        if !st.success() {
            return Err(anyhow!("launchctl bootstrap failed: {}", st));
        }
        let _ = std::process::Command::new("/bin/launchctl")
            .args(["enable", &format!("gui/{uid}/{LABEL}")])
            .status();
        let _ = std::process::Command::new("/bin/launchctl")
            .args(["kickstart", "-k", &format!("gui/{uid}/{LABEL}")])
            .status();
        println!("✓ installed + started: {}", plist_path.display());
        println!("  logs: {}/telegram-bot.{{out,err}}.log", logs_dir.display());
        println!("  status: crabcc-telegram status-service");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub fn uninstall() -> Result<()> {
        let plist_path = la_path()?;
        let uid = unsafe { libc_getuid() };
        let _ = std::process::Command::new("/bin/launchctl")
            .args(["bootout", &format!("gui/{uid}/{LABEL}")])
            .status();
        if plist_path.exists() {
            std::fs::remove_file(&plist_path).context("remove plist")?;
        }
        println!("✓ uninstalled: {}", plist_path.display());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub fn status() -> Result<()> {
        let uid = unsafe { libc_getuid() };
        let out = std::process::Command::new("/bin/launchctl")
            .args(["print", &format!("gui/{uid}/{LABEL}")])
            .output()
            .context("launchctl print")?;
        if !out.status.success() {
            println!("○ not loaded — run: crabcc-telegram install-service");
            return Ok(());
        }
        let body = String::from_utf8_lossy(&out.stdout);
        let pid = body
            .lines()
            .find(|l| l.contains("pid ="))
            .map(|l| l.trim().to_string())
            .unwrap_or_else(|| "(no pid line)".into());
        let last_exit = body
            .lines()
            .find(|l| l.contains("last exit code ="))
            .map(|l| l.trim().to_string())
            .unwrap_or_else(|| "(no last_exit_code line)".into());
        println!("● loaded as {LABEL}");
        println!("  {pid}");
        println!("  {last_exit}");
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn install() -> Result<()> {
        Err(anyhow!(
            "install-service: only macOS supported today. \
             For Linux: copy the systemd unit at \
             apps/crabcc-telegram/contrib/crabcc-telegram.service to \
             ~/.config/systemd/user/ and run `systemctl --user daemon-reload && \
             systemctl --user enable --now crabcc-telegram`."
        ))
    }
    #[cfg(not(target_os = "macos"))]
    pub fn uninstall() -> Result<()> {
        Err(anyhow!("uninstall-service: only macOS supported today."))
    }
    #[cfg(not(target_os = "macos"))]
    pub fn status() -> Result<()> {
        Err(anyhow!("status-service: only macOS supported today."))
    }

    fn home() -> Result<PathBuf> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME not set"))
    }

    fn la_path() -> Result<PathBuf> {
        Ok(home()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
    }

    // tiny libc shim — avoids pulling the full `libc` crate just for getuid.
    #[cfg(target_os = "macos")]
    unsafe fn libc_getuid() -> u32 {
        extern "C" {
            fn getuid() -> u32;
        }
        getuid()
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_owner_only_accepts_hardcoded_id() {
        assert!(is_owner(OWNER_TELEGRAM_USER_ID));
        assert!(!is_owner(0));
        assert!(!is_owner(1));
        assert!(!is_owner(OWNER_TELEGRAM_USER_ID.wrapping_add(1)));
        assert!(!is_owner(OWNER_TELEGRAM_USER_ID - 1));
    }

    #[test]
    fn truncate_handles_short_and_long() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abcdefghij", 4), "abcd");
    }
}
