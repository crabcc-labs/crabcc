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
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use teloxide::{
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, ParseMode, WebAppInfo},
    utils::command::BotCommands,
};
use tokio::io::AsyncReadExt;
use tokio::process::Command as TokioCommand;

// ── chat log (in-memory ring buffer for status web UI) ───────────────────────

#[derive(Clone, serde::Serialize)]
struct LogEntry {
    ts: u64,
    user: String,
    command: String,
    reply_preview: String,
}

type ChatLog = Arc<Mutex<Vec<LogEntry>>>;

fn new_chat_log() -> ChatLog {
    Arc::new(Mutex::new(Vec::new()))
}

fn log_entry(log: &ChatLog, user: &str, command: &str, reply_preview: &str) {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let entry = LogEntry {
        ts,
        user: user.to_string(),
        command: command.to_string(),
        reply_preview: reply_preview.chars().take(80).collect(),
    };
    if let Ok(mut l) = log.lock() {
        l.push(entry);
        if l.len() > 200 { l.remove(0); }   // ring buffer cap
    }
}

// ── status web server (port 8092) ─────────────────────────────────────────────

fn status_html(log: &[LogEntry], serve_url: &str) -> String {
    let rows: String = log.iter().rev().take(50).map(|e| {
        format!(
            "<tr><td>{}</td><td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
            e.ts, e.user, html_escape(&e.command), html_escape(&e.reply_preview)
        )
    }).collect();

    format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width,initial-scale=1"/>
  <title>CrabCC_bot</title>
  <meta http-equiv="refresh" content="5"/>
  <style>
    body{{font-family:monospace;background:#0d1117;color:#e6edf3;margin:2rem}}
    h1{{color:#58a6ff}}a{{color:#58a6ff}}
    table{{width:100%;border-collapse:collapse;margin-top:1rem}}
    th{{background:#161b22;text-align:left;padding:.4rem .6rem;color:#8b949e}}
    td{{padding:.35rem .6rem;border-bottom:1px solid #21262d;vertical-align:top}}
    code{{background:#161b22;padding:.1rem .3rem;border-radius:3px}}
    .badge{{display:inline-block;padding:.1rem .4rem;border-radius:3px;font-size:.75rem}}
    .ok{{background:#1f6f1f;color:#aff5af}}.warn{{background:#5a4000;color:#ffe066}}
    iframe{{width:100%;height:600px;border:1px solid #30363d;border-radius:6px;margin-top:1rem}}
  </style>
</head>
<body>
  <h1>🦀 CrabCC_bot — live status</h1>
  <p>
    <span class="badge ok">● running</span>
    &nbsp; <a href="{serve_url}/live" target="_blank">/live dashboard ↗</a>
    &nbsp; Auto-refreshes every 5 s.
  </p>
  <h2>Recent commands</h2>
  <table>
    <thead><tr><th>ts</th><th>user</th><th>command</th><th>reply preview</th></tr></thead>
    <tbody>{rows}</tbody>
  </table>
  <h2>Live dashboard</h2>
  <iframe src="{serve_url}/live" title="crabcc /live"></iframe>
</body>
</html>"#, serve_url = serve_url, rows = rows)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn run_status_server(log: ChatLog, serve_url: String, port: u16) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = match TcpListener::bind(format!("0.0.0.0:{port}")).await {
        Ok(l) => l,
        Err(e) => { tracing::error!("status server bind :{port} failed: {e}"); return; }
    };
    tracing::info!("status web UI at http://localhost:{port}");

    loop {
        let Ok((mut stream, _)) = listener.accept().await else { continue };
        let log = log.clone();
        let serve_url = serve_url.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf).await;
            let req = String::from_utf8_lossy(&buf);
            let path = req.split_whitespace().nth(1).unwrap_or("/");
            let (status, body) = if path == "/healthz" {
                ("200 OK", "ok".to_string())
            } else {
                let entries = log.lock().map(|l| l.clone()).unwrap_or_default();
                ("200 OK", status_html(&entries, &serve_url))
            };
            let ct = if path == "/healthz" { "text/plain" } else { "text/html; charset=utf-8" };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
        });
    }
}

// ── env config ────────────────────────────────────────────────────────────────

struct Config {
    serve_url: String,
    public_url: Option<String>,
    allowed_ids: Vec<i64>,
    status_port: u16,
}

impl Config {
    fn from_env() -> Self {
        Self {
            serve_url: std::env::var("CRABCC_SERVE_URL")
                .unwrap_or_else(|_| "http://localhost:8090".into()),
            public_url: std::env::var("CRABCC_PUBLIC_URL").ok(),
            allowed_ids: std::env::var("ALLOWED_TELEGRAM_IDS")
                .unwrap_or_default()
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect(),
            status_port: std::env::var("BOT_WEB_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8092),
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
        s
    } else {
        &s[..max]
    }
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
            let keyboard = if let Some(ref public_url) = cfg.public_url {
                let live_url = format!("{}/live", public_url);
                InlineKeyboardMarkup::new([
                    [InlineKeyboardButton::web_app(
                        "📊 Open live dashboard",
                        WebAppInfo { url: live_url.parse().unwrap() },
                    )],
                    [InlineKeyboardButton::url(
                        "🔗 Direct link",
                        format!("{}/live", cfg.serve_url).parse().unwrap(),
                    )],
                ])
            } else {
                // No HTTPS — plain URL button + hint to set CRABCC_PUBLIC_URL
                let live_url = format!("{}/live", cfg.serve_url);
                InlineKeyboardMarkup::new([[InlineKeyboardButton::url(
                    "📊 Open live dashboard (local only)",
                    live_url.parse().unwrap(),
                )]])
            };

            // Also send a text snapshot of current agent state
            let agents_url = format!("{}/api/agents", cfg.serve_url);
            let snapshot = match fetch_json(&agents_url).await {
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
    let log: ChatLog = new_chat_log();

    tracing::info!(
        serve_url = %cfg.serve_url,
        has_public_url = cfg.public_url.is_some(),
        allowed_ids = cfg.allowed_ids.len(),
        status_port = cfg.status_port,
        "CrabCC_bot starting"
    );

    // Start status web server (http://localhost:8092) in background
    {
        let log2 = log.clone();
        let url2 = cfg.serve_url.clone();
        let port = cfg.status_port;
        tokio::spawn(async move { run_status_server(log2, url2, port).await });
    }

    let bot = Bot::from_env();

    let log_for_handler = log.clone();
    let handler = Update::filter_message()
        .filter_command::<Cmd>()
        .endpoint(move |bot: Bot, msg: Message, cmd: Cmd| {
            let log = log_for_handler.clone();
            let cfg = Arc::clone(&cfg);
            async move {
                let user = msg.from().map(|u| u.first_name.clone()).unwrap_or_default();
                let cmd_str = format!("{cmd:?}");
                let result = handle(bot, msg, cmd, cfg).await;
                log_entry(&log, &user, &cmd_str, if result.is_ok() { "ok" } else { "err" });
                result
            }
        });

    Dispatcher::builder(bot, handler)
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}
