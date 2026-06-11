#!/usr/bin/env bash
# llama-server backend (binds 127.0.0.1:18080 by default). Front with lb-proxy.py on :8080.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# shellcheck source=/dev/null
[ -f "${ROOT}/lb/env.defaults" ] && source "${ROOT}/lb/env.defaults"
[ -f "${ROOT}/lb/env.local" ] && source "${ROOT}/lb/env.local"

MODEL_NAME="${1:-qwen-coder-32b}"
MODEL_PATH="models/${MODEL_NAME}.gguf"

if [ ! -e "$MODEL_PATH" ]; then
  echo "Model not found at $MODEL_PATH" >&2
  echo "Run: ./download-models.sh $MODEL_NAME" >&2
  exit 1
fi

THREADS="$(sysctl -n hw.perflevel0.physicalcpu 2>/dev/null || sysctl -n hw.physicalcpu)"
HOST="${LLAMA_BACKEND_HOST:-127.0.0.1}"
PORT="${LLAMA_BACKEND_PORT:-18080}"
CTX="${LLAMA_CTX:-8192}"
NGL="${LLAMA_NGL:-999}"
PARALLEL="${LLAMA_PARALLEL:-2}"

MLOCK_ARG=()
[ "${LLAMA_MLOCK:-1}" = "1" ] && MLOCK_ARG=(--mlock)

LLAMA_BIN="${LLAMA_BIN:-/opt/homebrew/bin/llama-server}"
if [ ! -x "$LLAMA_BIN" ]; then
  LLAMA_BIN="$(command -v llama-server || true)"
fi
[ -n "$LLAMA_BIN" ] || { echo "llama-server not found (brew install llama.cpp)" >&2; exit 1; }

echo "llama-server (backend)"
echo "  model:    $MODEL_NAME"
echo "  bind:     $HOST:$PORT"
echo "  ctx:      $CTX"
echo "  parallel: $PARALLEL slots (shared weights)"
echo "  ngl:      $NGL"
echo

exec "$LLAMA_BIN" \
  -m "$MODEL_PATH" \
  -ngl "$NGL" \
  -t "$THREADS" \
  -c "$CTX" \
  -np "$PARALLEL" \
  -cb \
  -fa on \
  --host "$HOST" \
  --port "$PORT" \
  --metrics \
  "${MLOCK_ARG[@]}"
