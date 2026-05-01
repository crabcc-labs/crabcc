#!/usr/bin/env bash
# cloudflared-tunnel.sh — managed cloudflared quick-tunnel for crabcc serve.
#
# Spawns cloudflared in the background, tails its log until the
# trycloudflare.com URL appears, and prints it. PID + URL state lives in
# ~/.crabcc/cloudflared.{pid,url}; the log lives next to the other
# Crabcc.app logs at ~/Library/Logs/Crabcc/cloudflared.log.
#
# Usage:
#   bash scripts/cloudflared-tunnel.sh start    # spawn + print URL (default)
#   bash scripts/cloudflared-tunnel.sh stop     # kill
#   bash scripts/cloudflared-tunnel.sh url      # print current URL
#   bash scripts/cloudflared-tunnel.sh status   # liveness check
#
# Env:
#   PORT  — local port to expose (default 8090, the crabcc serve port)

set -euo pipefail

STATE_DIR="${HOME}/.crabcc"
LOGS_DIR="${HOME}/Library/Logs/Crabcc"
PID_FILE="${STATE_DIR}/cloudflared.pid"
URL_FILE="${STATE_DIR}/cloudflared.url"
LOG_FILE="${LOGS_DIR}/cloudflared.log"
PORT="${PORT:-8090}"

mkdir -p "$STATE_DIR" "$LOGS_DIR"

is_running() {
    [[ -f "$PID_FILE" ]] || return 1
    local pid
    pid=$(cat "$PID_FILE")
    kill -0 "$pid" 2>/dev/null
}

cmd_start() {
    if is_running; then
        echo "already running (pid $(cat "$PID_FILE"))"
        cmd_url
        return 0
    fi

    command -v cloudflared >/dev/null 2>&1 || {
        echo "cloudflared not on PATH. Install: brew install cloudflared" >&2
        exit 1
    }

    : > "$LOG_FILE"
    # nohup detaches from this shell; setsid would too but isn't on macOS by default.
    nohup cloudflared tunnel --url "http://localhost:${PORT}" \
        >"$LOG_FILE" 2>&1 &
    local pid=$!
    echo "$pid" > "$PID_FILE"
    echo "starting cloudflared (pid $pid, port $PORT)…"

    # Bounded wait — cloudflared usually prints the URL within 3-8s.
    for _ in $(seq 1 30); do
        local url
        url=$(grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' "$LOG_FILE" 2>/dev/null | head -n1) || true
        if [[ -n "$url" ]]; then
            echo "$url" > "$URL_FILE"
            cat <<EOF

✓ tunnel up: $url

  log:           $LOG_FILE
  apps/crabcc-telegram/.env:
                 CRABCC_PUBLIC_URL=$url
  BotFather Mini App URL:
                 $url/?role=mini
EOF
            return 0
        fi
        sleep 1
    done

    echo "✗ no trycloudflare.com URL after 30s — check $LOG_FILE" >&2
    return 1
}

cmd_stop() {
    if ! is_running; then echo "not running"; rm -f "$PID_FILE" "$URL_FILE"; return 0; fi
    local pid
    pid=$(cat "$PID_FILE")
    kill "$pid" 2>/dev/null || true
    rm -f "$PID_FILE" "$URL_FILE"
    echo "stopped (pid $pid)"
}

cmd_url() {
    if [[ -f "$URL_FILE" ]]; then cat "$URL_FILE"; else
        echo "no URL recorded — run 'start' first" >&2; return 1
    fi
}

cmd_status() {
    if is_running; then
        echo "running (pid $(cat "$PID_FILE"))"
        [[ -f "$URL_FILE" ]] && echo "url: $(cat "$URL_FILE")"
    else
        echo "not running"
    fi
}

case "${1:-start}" in
    start)  cmd_start ;;
    stop)   cmd_stop ;;
    url)    cmd_url ;;
    status) cmd_status ;;
    *)      echo "usage: $0 {start|stop|url|status}" >&2; exit 1 ;;
esac
