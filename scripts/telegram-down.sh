#!/usr/bin/env bash
# telegram-down.sh — counterpart to telegram-up.sh.
#
# Stops + removes the crabcc-telegram container. By default leaves the
# cloudflared tunnel alive (other things may be using it — `task viz`,
# the web dashboard, ad-hoc curl debugging). Pass `--with-tunnel` to
# stop the tunnel too.
#
# Idempotent: re-running on a clean system is a no-op.

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
STATE_DIR="$REPO_ROOT/.crabcc"
STATE_FILE="$STATE_DIR/telegram-up.json"
TUNNEL_SCRIPT="$SCRIPT_DIR/cloudflared-tunnel.sh"
CONTAINER_NAME="crabcc-telegram"

WITH_TUNNEL=0
for arg in "$@"; do
    case "$arg" in
        --with-tunnel) WITH_TUNNEL=1 ;;
        -h|--help)
            cat <<EOF
usage: $(basename "$0") [--with-tunnel]

  Stops + removes the $CONTAINER_NAME container.
  --with-tunnel   Also stop the cloudflared quick-tunnel.
EOF
            exit 0
            ;;
        *) printf 'unknown arg: %s\n' "$arg" >&2; exit 2 ;;
    esac
done

log()  { printf '\033[36m[telegram-down]\033[0m %s\n' "$*" >&2; }
ok()   { printf '\033[32m[telegram-down]\033[0m ✓ %s\n' "$*" >&2; }

# ── container ────────────────────────────────────────────────────────────

if docker inspect "$CONTAINER_NAME" >/dev/null 2>&1; then
    log "removing container $CONTAINER_NAME"
    docker rm -f "$CONTAINER_NAME" >/dev/null
    ok "container removed"
else
    ok "container not running (nothing to do)"
fi

# ── state file ───────────────────────────────────────────────────────────

if [[ -f "$STATE_FILE" ]]; then
    rm -f "$STATE_FILE"
    ok "state file cleared"
fi

# ── tunnel (opt-in) ──────────────────────────────────────────────────────

if (( WITH_TUNNEL )); then
    log "stopping cloudflared tunnel"
    bash "$TUNNEL_SCRIPT" stop || true
    ok "tunnel stopped"
else
    if [[ -f "$HOME/.crabcc/cloudflared.url" ]]; then
        ok "tunnel left alive ($(cat "$HOME/.crabcc/cloudflared.url")) — pass --with-tunnel to stop"
    fi
fi
