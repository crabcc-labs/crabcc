#!/usr/bin/env bash
# Start/stop llama pool on m3: backend (llama-server) + LB (lb-proxy.py).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LB_DIR="${ROOT}/lb"
TMUX="${TMUX_BIN:-/opt/homebrew/bin/tmux}"
MODEL="${LLAMA_MODEL:-qwen-coder-32b}"

# shellcheck source=/dev/null
[ -f "${LB_DIR}/env.defaults" ] && source "${LB_DIR}/env.defaults"
[ -f "${LB_DIR}/env.local" ] && source "${LB_DIR}/env.local"

usage() {
  echo "usage: $0 {start|stop|restart|start-lb|status|logs}" >&2
  exit 2
}

tmux_() {
  if [ -x "$TMUX" ]; then
    "$TMUX" "$@"
  else
    command tmux "$@"
  fi
}

mem_ok() {
  local page_size free_pages inactive_pages avail_mb need_mb
  page_size="$(sysctl -n hw.pagesize)"
  read -r free_pages inactive_pages < <(
    vm_stat | awk '
      /Pages free/     { gsub(/\./,"",$3); f=$3 }
      /Pages inactive/ { gsub(/\./,"",$3); i=$3 }
      END { print f+0, i+0 }
    '
  )
  avail_mb=$(( (free_pages + inactive_pages) * page_size / 1024 / 1024 ))
  need_mb="${LLAMA_MIN_AVAIL_MB:-22000}"
  if [ "$avail_mb" -lt "$need_mb" ]; then
    echo "refusing start: ~${avail_mb}MB reclaimable (need >= ${need_mb}MB)" >&2
    echo "  stop stray llama/opencode or use LLAMA_MODEL=qwen-coder-14b" >&2
    return 1
  fi
  echo "mem_ok: ~${avail_mb}MB reclaimable (free+inactive)"
}

kill_stray_llama() {
  pkill -f '[l]lama-server' 2>/dev/null || true
  sleep 2
}

stop_sessions() {
  tmux_ kill-session -t llama-lb 2>/dev/null || true
  tmux_ kill-session -t llama-be 2>/dev/null || true
  tmux_ kill-session -t llama 2>/dev/null || true
}

start_backend() {
  mem_ok
  tmux_ kill-session -t llama-be 2>/dev/null || true
  tmux_ new -d -s llama-be \
    "export PATH=/opt/homebrew/bin:\$PATH && cd $(printf %q "$ROOT") && exec ./lb/run-backend.sh $(printf %q "$MODEL") 2>&1 | tee -a logs/backend.log"
}

start_lb() {
  local tries=0
  until curl -fsS --max-time 2 "http://127.0.0.1:${LLAMA_BACKEND_PORT:-18080}/health" >/dev/null 2>&1; do
    tries=$((tries + 1))
    [ "$tries" -le 60 ] || { echo "backend /health timeout" >&2; return 1; }
    sleep 2
  done
  tmux_ kill-session -t llama-lb 2>/dev/null || true
  tmux_ new -d -s llama-lb \
    "set -a && . $(printf %q "${LB_DIR}/env.defaults") && [ -f $(printf %q "${LB_DIR}/env.local") ] && . $(printf %q "${LB_DIR}/env.local"); set +a; cd $(printf %q "$LB_DIR") && exec python3 ./lb-proxy.py 2>&1 | tee -a ../logs/lb.log"
  local t=0
  until curl -fsS --max-time 2 "http://127.0.0.1:${LLAMA_LB_PORT:-8080}/health" >/dev/null 2>&1; do
    t=$((t + 1))
    [ "$t" -le 30 ] || { echo "lb /health timeout" >&2; return 1; }
    sleep 1
  done
  echo "pool up: http://127.0.0.1:${LLAMA_LB_PORT:-8080}/health"
}

cmd="${1:-}"
case "$cmd" in
  start)
    kill_stray_llama
    mkdir -p "${ROOT}/logs"
    start_backend
    start_lb
    ;;
  stop)
    stop_sessions
    kill_stray_llama
    ;;
  restart)
    stop_sessions
    kill_stray_llama
    sleep 2
    mkdir -p "${ROOT}/logs"
    start_backend
    start_lb
    ;;
  start-lb)
    start_lb
    ;;
  status)
    tmux_ ls 2>/dev/null || echo "(no tmux sessions)"
    if curl -fsS --max-time 2 "http://127.0.0.1:${LLAMA_LB_PORT:-8080}/lb/status" >/dev/null 2>&1; then
      echo "lb: ok"
      curl -fsS --max-time 2 "http://127.0.0.1:${LLAMA_LB_PORT:-8080}/lb/status" 2>/dev/null || true
    elif curl -fsS --max-time 2 "http://127.0.0.1:${LLAMA_LB_PORT:-8080}/health" >/dev/null 2>&1; then
      echo "lb: ok (health only)"
    else
      echo "lb: down"
    fi
    curl -fsS --max-time 2 "http://127.0.0.1:${LLAMA_BACKEND_PORT:-18080}/health" 2>/dev/null && echo "backend: ok" || echo "backend: down"
    oc="$(ps aux 2>/dev/null | grep -c '[o]pencode run' || echo 0)"
    echo "opencode jobs: ${oc} (keep <= $((LLAMA_MAX_INFLIGHT + LLAMA_MAX_QUEUE)) for smooth queue)"
    ;;
  logs)
    tmux_ capture-pane -t llama-be -p 2>/dev/null | tail -15 || true
    echo "---"
    tmux_ capture-pane -t llama-lb -p 2>/dev/null | tail -10 || true
    ;;
  *)
    usage
    ;;
esac
