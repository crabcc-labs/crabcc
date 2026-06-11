#!/usr/bin/env bash
# Rsync install/m3-llama/ into /opt/plodri/llama-server/lb/ on the M3 Mac.
set -euo pipefail

REMOTE="${M3_LLAMA_REMOTE:-m3-task}"
TARGET="${M3_LLAMA_TARGET:-/opt/plodri/llama-server}"
SRC="$(cd "$(dirname "$0")/../install/m3-llama" && pwd)"

chmod +x "${SRC}/pool.sh" "${SRC}/run-backend.sh" "${SRC}/lb-proxy.py"

rsync -av \
  "${SRC}/env.defaults" \
  "${SRC}/run-backend.sh" \
  "${SRC}/lb-proxy.py" \
  "${SRC}/pool.sh" \
  "${SRC}/README.md" \
  "${REMOTE}:${TARGET}/lb/"

ssh "$REMOTE" "chmod +x ${TARGET}/lb/pool.sh ${TARGET}/lb/run-backend.sh ${TARGET}/lb/lb-proxy.py"
echo "deployed → ${REMOTE}:${TARGET}/lb/"
echo "restart: ssh ${REMOTE} 'cd ${TARGET} && ./lb/pool.sh restart'"
