#!/usr/bin/env bash
# crabcc Ollama auth stack — launcher
#
# Priority: Apple Containers (macOS 26) → Docker → self-hosted hint
#
# Usage:
#   ./start.sh            # bring up the stack
#   ./start.sh --down     # tear down the stack
#   ./start.sh --status   # show stack status
#   ./start.sh --pull     # pull/refresh models without restarting
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
CRABCC_SCRIPTS="$(cd "$SCRIPT_DIR/../../scripts" 2>/dev/null && pwd)" || CRABCC_SCRIPTS=""

ACTION="up"
for arg in "$@"; do
  case "$arg" in
    --down)   ACTION="down" ;;
    --status) ACTION="status" ;;
    --pull)   ACTION="pull" ;;
    -h|--help)
      sed -n '1,15p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown arg: $arg" >&2; exit 1 ;;
  esac
done

# ── 1. Preflight: general dev deps ──────────────────────────────────────────
if [ -n "$CRABCC_SCRIPTS" ] && [ -x "$CRABCC_SCRIPTS/check-deps.sh" ]; then
  echo "▶ Checking deps..."
  "$CRABCC_SCRIPTS/check-deps.sh" --quiet || true
fi

# ── 2. Pick container runtime: Apple Containers → Docker ────────────────────
if command -v container &>/dev/null; then
  COMPOSE="container compose"
  RUNTIME="Apple Containers"
elif command -v docker &>/dev/null; then
  COMPOSE="docker compose"
  RUNTIME="Docker"
else
  cat <<EOF

No container runtime found.

  Apple Containers: built into macOS 26 (run: container --version)
  Docker Desktop:   https://docs.docker.com/desktop/mac/

Fallback — self-hosted ollama (no LiteLLM caching):
  brew install ollama
  ollama serve &
  ollama pull qwen3.5:35b-a3b-coding-nvfp4
  ollama pull qwen2.5-coder
  Then in free-claude-code/.env set:
    OLLAMA_BASE_URL=http://localhost:11434
EOF
  exit 1
fi
echo "▶ Runtime: $RUNTIME"

# ── 3. Handle --down / --status / --pull ────────────────────────────────────
cd "$SCRIPT_DIR"

if [ "$ACTION" = "down" ]; then
  echo "▶ Tearing down stack..."
  $COMPOSE down
  exit 0
fi

if [ "$ACTION" = "status" ]; then
  $COMPOSE ps
  exit 0
fi

# ── 4. Ensure .env has real keys ────────────────────────────────────────────
ENV_FILE="$SCRIPT_DIR/.env"
if [ ! -f "$ENV_FILE" ] || grep -q "changeme" "$ENV_FILE" 2>/dev/null; then
  echo "▶ Generating keys..."
  "$SCRIPT_DIR/init-keys.sh" --quiet >/dev/null
fi

# ── 5. Start or pull models ─────────────────────────────────────────────────
if [ "$ACTION" = "pull" ]; then
  echo "▶ Pulling models into running container..."
  $COMPOSE exec ollama ollama pull qwen3.5:35b-a3b-coding-nvfp4
  $COMPOSE exec ollama ollama pull qwen2.5-coder
  exit 0
fi

# ── 6. Bring up ─────────────────────────────────────────────────────────────
echo "▶ Starting stack..."
$COMPOSE up -d --wait

# Pull models on first start (non-fatal — they may already exist in the volume)
echo "▶ Pulling models (skipped if already present)..."
$COMPOSE exec ollama ollama pull qwen3.5:35b-a3b-coding-nvfp4 2>/dev/null || true
$COMPOSE exec ollama ollama pull qwen2.5-coder 2>/dev/null || true

# ── 7. Print connection info ─────────────────────────────────────────────────
MASTER_KEY="$(grep -m1 '^LITELLM_MASTER_KEY=' "$ENV_FILE" | cut -d= -f2- || true)"
cat <<EOF

Stack ready  ($RUNTIME):
  :4000   LiteLLM proxy    — OpenAI-compat + prompt caching
  :11435  Caddy → Ollama   — Bearer-auth gated

Add to free-claude-code/.env to route through LiteLLM:
  OLLAMA_BASE_URL=http://localhost:4000
  OLLAMA_API_KEY=${MASTER_KEY:-<see $ENV_FILE for LITELLM_MASTER_KEY>}
EOF
