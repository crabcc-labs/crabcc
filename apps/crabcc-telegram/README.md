# CrabCC_bot — Telegram interface

Telegram bot for running crabcc agents, checking status, and viewing the live
dashboard as a Telegram Mini App. Issue #134.

## Quick start

```bash
# Set token (gitignored .env)
echo 'TELEGRAM_BOT_TOKEN=<your-token>' >> .env
task telegram-bot
```

## Commands

| Command | What it does |
|---------|-------------|
| `/agent <task>` | Launch crabcc agent via ollama backend |
| `/status` | List 5 most recent agent runs |
| `/doctor` | Run all doctor checks, report failures |
| `/search <query>` | Search memory, show top 5 results |
| `/dashboard` | Open /live as Mini App or plain link |
| `/kill <id>` | Kill running agent (with confirmation) |
| `/index` | Re-index current repo |

## Live dashboard as Mini App

Telegram Mini Apps require **HTTPS**. Expose `crabcc serve` publicly with one
of these:

```bash
# Option A — ngrok (free tier, ephemeral URL)
ngrok http 8090
# Output: https://xxxx.ngrok.io → set CRABCC_PUBLIC_URL=https://xxxx.ngrok.io

# Option B — cloudflared (free, stable with custom domain)
cloudflared tunnel --url http://localhost:8090
# Output: https://random-name.trycloudflare.com

# Option C — tailscale funnel (if on Tailscale)
tailscale funnel 8090
```

Then set `CRABCC_PUBLIC_URL` in your `.env`:
```
CRABCC_PUBLIC_URL=https://xxxx.ngrok.io
```

The `/dashboard` command will then show an **Open App** button that opens
`/live` inside Telegram's built-in browser.

### Mini App init-data validation

If you want to validate the `initData` sent by Telegram to the Mini App
(for server-side auth), use `init-data-rs`:
https://github.com/escwxyz/init-data-rs

The `/live` endpoint in `crabcc serve` can validate `window.Telegram.WebApp.initData`
against `TELEGRAM_BOT_TOKEN` to ensure only authenticated Telegram users can
access the dashboard.

### Mini App tech stack references

- Official Telegram Mini Apps docs: https://core.telegram.org/bots/webapps
- Awesome Telegram Mini Apps: https://github.com/telegram-mini-apps-dev/awesome-telegram-mini-apps
- Community insights on Mini App architecture: see reddit/medium links in the
  commit message for tradeoffs

## Access control

Restrict the bot to specific Telegram user IDs:
```
ALLOWED_TELEGRAM_IDS=123456789,987654321
```

Find your Telegram user ID: message `@userinfobot` in Telegram.

## Environment variables

| Var | Required | Default | Description |
|-----|----------|---------|-------------|
| `TELEGRAM_BOT_TOKEN` | Yes | — | From BotFather |
| `CRABCC_SERVE_URL` | No | `http://localhost:8090` | crabcc serve endpoint |
| `CRABCC_PUBLIC_URL` | No | — | HTTPS URL for Mini App button |
| `ALLOWED_TELEGRAM_IDS` | No | open | Comma-separated user IDs |

## Building standalone

This crate is excluded from the workspace due to a nightly libc ICE.
Build with stable Rust:

```bash
cd apps/crabcc-telegram
cargo build --release   # uses stable via .cargo/config.toml
```

Or via Taskfile from the repo root:
```bash
task telegram-bot       # cargo run --release
task telegram-bot-dev   # RUST_LOG=debug cargo run
```
