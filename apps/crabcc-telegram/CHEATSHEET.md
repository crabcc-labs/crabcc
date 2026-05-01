# crabcc-telegram — Quick-Start Cheatsheet

> Bot at `apps/crabcc-telegram/`. Owner-locked to Telegram user **`5875395828`** (hardcoded in `src/main.rs:OWNER_TELEGRAM_USER_ID`). Anyone else gets silently rejected.

## TL;DR — bot is silent, what now

```bash
# 1. Is it running?
launchctl print "gui/$(id -u)/com.crabcc.telegram-bot" | head -20

# 2. What did it last say?
tail -n 100 ~/Library/Logs/Crabcc/telegram-bot.err.log
tail -n 100 ~/Library/Logs/Crabcc/telegram-bot.out.log

# 3. Is the backend up?
curl -sf http://localhost:8090/api/agents | head -5 || echo "crabcc serve is DOWN"

# 4. Token still valid?
grep TELEGRAM_BOT_TOKEN ~/workspace/bin/crabcc/apps/crabcc-telegram/.env
curl -s "https://api.telegram.org/bot$(grep -E '^TELEGRAM_BOT_TOKEN=' ~/workspace/bin/crabcc/apps/crabcc-telegram/.env | cut -d= -f2-)/getMe" | jq

# 5. Are you messaging from the owner account?
#    Send `/start` to @userinfobot in Telegram → confirm your ID == 5875395828
```

## First-time install

```bash
cd ~/workspace/bin/crabcc/apps/crabcc-telegram
./setup.sh                  # prompts for TELEGRAM_BOT_TOKEN, builds, installs LaunchAgent, tails logs
```

`setup.sh` does all four steps:
1. writes `.env` (mode 0600) with your `TELEGRAM_BOT_TOKEN`
2. `cargo build --release` (uses stable per `.cargo/config.toml` — workspace excluded)
3. copies binary to `~/.cargo/bin/crabcc-telegram`
4. `crabcc-telegram install-service` registers LaunchAgent

## Daily ops

```bash
# Service control
launchctl kickstart -k "gui/$(id -u)/com.crabcc.telegram-bot"   # restart (preserves config)
launchctl bootout    "gui/$(id -u)/com.crabcc.telegram-bot"     # stop (unload)
crabcc-telegram install-service                                 # (re-)load LaunchAgent

# Live logs (both streams)
tail -f ~/Library/Logs/Crabcc/telegram-bot.{out,err}.log

# Foreground run — best when debugging (skips LaunchAgent)
launchctl bootout "gui/$(id -u)/com.crabcc.telegram-bot" 2>/dev/null
cd ~/workspace/bin/crabcc/apps/crabcc-telegram
RUST_LOG=debug TELEGRAM_BOT_TOKEN=$(grep TELEGRAM_BOT_TOKEN .env | cut -d= -f2-) cargo run --release
# (or: task telegram-bot-dev from repo root)
```

## Bot commands (DM the bot from your owner account)

| Command | Effect |
|---|---|
| `/agent <task>` | Launch crabcc agent via ollama |
| `/status` | Last 5 agent runs |
| `/doctor` | All doctor checks; reports failures |
| `/search <q>` | Memory search, top 5 |
| `/dashboard` | `/live` as Mini App (needs `CRABCC_PUBLIC_URL`) or plain link |
| `/kill <id>` | Kill running agent (with confirm) |
| `/index` | Re-index current repo |

## Environment (`apps/crabcc-telegram/.env`)

| Var | Required | Default | Notes |
|---|---|---|---|
| `TELEGRAM_BOT_TOKEN` | yes | — | from @BotFather |
| `CRABCC_SERVE_URL` | no | `http://localhost:8090` | backend |
| `CRABCC_PUBLIC_URL` | no | — | HTTPS tunnel for `/dashboard` Mini App |

## Mini App tunnel (only if you use `/dashboard`)

Telegram Mini Apps require HTTPS. Pick one:

```bash
ngrok http 8090                                       # ephemeral, free
cloudflared tunnel --url http://localhost:8090        # stable w/ custom domain
tailscale funnel 8090                                 # if you're on Tailscale
```

Add the resulting URL to `.env`:
```
CRABCC_PUBLIC_URL=https://xxxx.ngrok.io
```

Restart the bot after editing `.env`.

## Silence troubleshooting matrix

| Symptom | Likely cause | Fix |
|---|---|---|
| `getMe` returns 401 | bad/revoked token | re-issue via @BotFather → update `.env` → restart |
| `getMe` OK, no replies | not the owner ID | message @userinfobot, confirm == `5875395828` (or edit const + rebuild) |
| `getMe` OK, owner OK, no replies | LaunchAgent dead | `launchctl print …` → check exit reason → kickstart |
| Bot replies "backend unreachable" | `crabcc serve` not running | start it (`crabcc serve` or its LaunchAgent) |
| `/dashboard` shows plain link only | `CRABCC_PUBLIC_URL` unset or HTTP | set HTTPS tunnel URL → restart |
| Logs show `connect timeout` repeatedly | mobile network blip | exponential-backoff retry kicks in (3 attempts); persist? check serve URL reachability |
| Builds break with libc ICE | stale nightly toolchain | this crate is **outside the workspace** intentionally; ensure `.cargo/config.toml` pins stable |

## File map

```
apps/crabcc-telegram/
├── Cargo.toml                # standalone workspace (NOT in /Cargo.toml)
├── README.md                 # spec
├── CHEATSHEET.md             # this file
├── setup.sh                  # one-shot installer
├── telegram_setup.log        # last setup.sh run
├── .env                      # 0600, gitignored — TELEGRAM_BOT_TOKEN + tunnel URL
├── .cargo/config.toml        # pins stable
└── src/main.rs               # single file; OWNER_TELEGRAM_USER_ID at top
```

## "I want to grant another account access"

Don't widen this bot. Stand up a second bot (new BotFather token, separate `.env`, separate LaunchAgent label) pointed at the same `crabcc serve`. The single-owner lockdown is intentional — env can't override.
