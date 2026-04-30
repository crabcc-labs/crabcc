//! CrabCC_bot — Telegram interface for crabcc agents.
//!
//! Issue #134. Uses teloxide (https://github.com/teloxide/teloxide).
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
//!   ALLOWED_TELEGRAM_IDS — comma-separated Telegram user IDs allowed to send
//!                          commands (empty = open to anyone, not recommended)
//!
//! # Run
//!   TELEGRAM_BOT_TOKEN=<token> cargo run -p crabcc-telegram
//!   # or via Taskfile:
//!   task telegram-bot

use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result};
use reqwest::Url;
use teloxide::{
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, ParseMode, WebAppInfo},
    utils::command::BotCommands,
};
use tokio::io::AsyncReadExt;
use tokio::process::Command as TokioCommand;

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
    allowed_ids: Vec<i64>,
}

impl Config {
    fn from_env() -> Self {
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

        Self {
            serve_url,
            public_url,
            serve_live_url,
            public_live_url,
            agents_api_url,
            allowed_ids: std::env::var("ALLOWED_TELEGRAM_IDS")
                .unwrap_or_default()
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect(),
        }
    }

    fn is_allowed(&self, user_id: i64) -> bool {
        self.allowed_ids.is_empty() || self.allowed_ids.contains(&user_id)
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
        .stderr(Stdio::piped());
    let child = cmd.spawn().context("spawn crabcc")?;
    let out = child.wait_with_output().await.context("wait")?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if out.status.success() {
        Ok(if stdout.is_empty() { "Done.".into() } else { stdout })
    } else {
        Ok(format!("error:\n{}", if stderr.is_empty() { stdout } else { stderr }))
    }
}

async fn fetch_json(url: &str) -> Result<serde_json::Value> {
    let resp = reqwest::get(url).await?.json().await?;
    Ok(resp)
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
    let user_id = msg.from().map(|u| u.id.0 as i64).unwrap_or(0);
    if !cfg.is_allowed(user_id) {
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
            let snapshot = match fetch_json(cfg.agents_api_url.as_str()).await {
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
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("crabcc_telegram=info".parse().unwrap()),
        )
        .init();

    let cfg = Arc::new(Config::from_env());

    tracing::info!(
        serve_url = %cfg.serve_url,
        has_public_url = cfg.public_url.is_some(),
        allowed_ids = cfg.allowed_ids.len(),
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
}
