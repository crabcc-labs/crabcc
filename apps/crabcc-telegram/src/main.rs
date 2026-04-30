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
use teloxide::{
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, ParseMode, WebAppInfo},
    utils::command::BotCommands,
};
use tokio::io::AsyncReadExt;
use tokio::process::Command as TokioCommand;

// ── env config ────────────────────────────────────────────────────────────────

struct Config {
    serve_url: String,
    public_url: Option<String>,
    allowed_ids: Vec<i64>,
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
