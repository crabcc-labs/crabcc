#!/usr/bin/env bash
# Dispatch a bounded coding subtask to m3 via SSH mux + opencode (local-llama).
#
# Reach: Host m3 in ~/.ssh/config must LocalForward 8080 → m3:8080.
#   ssh -fN m3   # once per laptop boot (or when forward dies)
#
# Prereqs (m3):
#   - llama pool: ./lb/pool.sh (LB :8080 → backend :18080; see install/m3-llama/)
#   - opencode 1.15+ with provider local-llama → http://127.0.0.1:8080/v1
#
# Prereqs (this machine): jq, Host m3-task (ControlMaster)
#
# Usage:
#   ./scripts/dispatch-m3.sh "Add a unit test for foo() in bar.rs"
#   DIR=/path/on/m3 ./scripts/dispatch-m3.sh "task text"

set -euo pipefail

TASK="${1:-}"
DIR="${DIR:-/tmp}"
MODEL="${M3_OPENCODE_MODEL:-local-llama/qwen-coder-32b}"
HEALTH_URL="${M3_HEALTH_URL:-http://127.0.0.1:8080/health}"
HEALTH_MAX="${M3_HEALTH_MAX:-3}"

if [[ -z "$TASK" ]]; then
  echo "usage: $0 \"<one-paragraph task>\"" >&2
  exit 2
fi

command -v jq >/dev/null || { echo "jq required" >&2; exit 10; }

# Tunnel llama API to this laptop (ExitOnForwardFailure on Host m3).
if ! curl -fsS --max-time 1 "$HEALTH_URL" >/dev/null 2>&1; then
  ssh -fN m3 2>/dev/null || true
fi
curl -fsS --max-time "$HEALTH_MAX" "$HEALTH_URL" >/dev/null \
  || { echo "m3 llama health failed: $HEALTH_URL (run: ssh -fN m3; ssh m3-task 'cd /opt/plodri/llama-server && ./lb/pool.sh restart')" >&2; exit 11; }

ssh -O check m3-task >/dev/null 2>&1 || ssh -fN m3-task

ssh m3-task "PATH=\$HOME/.opencode/bin:/opt/homebrew/bin:\$PATH \
  opencode run --pure --format json --log-level ERROR \
  --dangerously-skip-permissions \
  -m $(printf %q "$MODEL") --dir $(printf %q "$DIR") \
  $(printf %q "$TASK")" \
| jq -c --unbuffered '
    select(.type=="message.delta" or .type=="tool.result" or .type=="error")
    | {t:.type, d:(.delta // .result // .error)}'
