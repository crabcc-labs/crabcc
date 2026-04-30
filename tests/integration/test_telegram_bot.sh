#!/usr/bin/env bash
# tests/integration/test_telegram_bot.sh — Telegram bot smoke + e2e test
#
# Tests two layers:
#   1. Bot API connectivity — can we reach the Telegram API with the token?
#   2. Live e2e — sends /status to the bot and checks for a reply.
#      Requires the bot to be running locally (task telegram-bot).
#
# Usage:
#   bash tests/integration/test_telegram_bot.sh           # connectivity only
#   LIVE=1 bash tests/integration/test_telegram_bot.sh    # full e2e
#
# Exit: 0 pass, 1 fail, 2 prerequisites missing
set -euo pipefail

TOKEN="${TELEGRAM_BOT_TOKEN:?TELEGRAM_BOT_TOKEN not set — add to .env or export}"
CHAT="${TELEGRAM_CHAT_ID:?TELEGRAM_CHAT_ID not set — run: task telegram-setup}"
LIVE="${LIVE:-0}"
API="https://api.telegram.org/bot${TOKEN}"
PASS=0; FAIL=0

ok()   { PASS=$((PASS+1)); printf "  \033[32m✓\033[0m %s\n" "$*"; }
fail() { FAIL=$((FAIL+1)); printf "  \033[31m✗\033[0m %s\n" "$*"; }

echo "Telegram bot integration test"
echo ""

command -v python3 >/dev/null || { echo "python3 missing"; exit 2; }

# ── 1. API connectivity ───────────────────────────────────────────────────────
echo "── API connectivity ──────────────────────────────────────────────────────"

ME=$(curl -sf "${API}/getMe" 2>/dev/null || echo "{}")
BOT_OK=$(printf '%s' "$ME" | python3 -c "import sys,json; d=json.load(sys.stdin); print('yes' if d.get('ok') else 'no')" 2>/dev/null)
if [ "$BOT_OK" = "yes" ]; then
  BOT_NAME=$(printf '%s' "$ME" | python3 -c "import sys,json; print(json.load(sys.stdin)['result']['username'])" 2>/dev/null)
  ok "getMe → @${BOT_NAME:-CrabCC_bot}"
else
  fail "getMe failed — check TELEGRAM_BOT_TOKEN"
fi

# ── 2. getUpdates is reachable ────────────────────────────────────────────────
UPD=$(curl -sf "${API}/getUpdates?limit=1" 2>/dev/null || echo "{}")
UPD_OK=$(printf '%s' "$UPD" | python3 -c "import sys,json; print('yes' if json.load(sys.stdin).get('ok') else 'no')" 2>/dev/null)
[ "$UPD_OK" = "yes" ] && ok "getUpdates reachable" || fail "getUpdates failed"

# ── 3. sendMessage (to ourselves) ─────────────────────────────────────────────
SEND=$(curl -sf "${API}/sendMessage" \
  -d "chat_id=${CHAT}&text=crabcc+bot+smoke+test+$(date +%s)" 2>/dev/null || echo "{}")
SEND_OK=$(printf '%s' "$SEND" | python3 -c "import sys,json; print('yes' if json.load(sys.stdin).get('ok') else 'no')" 2>/dev/null)
[ "$SEND_OK" = "yes" ] && ok "sendMessage → delivered to chat $CHAT" || fail "sendMessage failed"

# ── 4. Live e2e (optional — requires bot running) ─────────────────────────────
if [ "$LIVE" = "1" ]; then
  echo ""
  echo "── live e2e (LIVE=1) ─────────────────────────────────────────────────────"

  BEFORE_ID=$(curl -sf "${API}/getUpdates?limit=1&offset=-1" 2>/dev/null | \
    python3 -c "import sys,json; r=json.load(sys.stdin)['result']; print(r[-1]['update_id'] if r else 0)" 2>/dev/null || echo 0)

  # Send /status command
  curl -sf "${API}/sendMessage" \
    -d "chat_id=${CHAT}&text=/status" >/dev/null
  ok "sent /status to bot"

  # Wait up to 10 s for a reply
  REPLIED=0
  for i in $(seq 1 10); do
    sleep 1
    REPLY=$(curl -sf "${API}/getUpdates?offset=$((BEFORE_ID+1))&limit=5" 2>/dev/null | \
      python3 -c "
import sys,json
d=json.load(sys.stdin)
bot_msgs=[u['message'] for u in d['result']
          if u.get('message',{}).get('from',{}).get('is_bot')]
print(bot_msgs[-1]['text'][:100] if bot_msgs else '')
" 2>/dev/null)
    if [ -n "$REPLY" ]; then
      REPLIED=1
      ok "/status reply: ${REPLY:0:80}…"
      break
    fi
  done
  [ "$REPLIED" -eq 1 ] || fail "/status — no bot reply within 10 s (is `task telegram-bot` running?)"

  # Send /doctor
  curl -sf "${API}/sendMessage" -d "chat_id=${CHAT}&text=/doctor" >/dev/null
  ok "sent /doctor to bot"
fi

echo ""
echo "── summary ───────────────────────────────────────────────────────────────"
echo "  passed: $PASS   failed: $FAIL"
echo ""
[ "$FAIL" -eq 0 ] || { echo "Run with LIVE=1 for full e2e (requires task telegram-bot)"; false; }
