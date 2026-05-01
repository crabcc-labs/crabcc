#!/usr/bin/env bash
# Start the full crabcc-agents stack with the host's Anthropic key
# propagated through to the LiteLLM proxy.
#
# Usage:
#   ./scripts/start.sh
#   # or, with explicit override:
#   ANTHROPIC_API_KEY=sk-ant-... ./scripts/start.sh

set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
crate_dir="$(cd "${dir}/.." && pwd)"

# Pull the key from host Claude if not already set.
if [[ -z "${ANTHROPIC_API_KEY:-}" ]]; then
  if key="$("${dir}/extract-anthropic-key.sh" 2>/tmp/crabcc-key-extract.log)"; then
    export ANTHROPIC_API_KEY="$key"
    echo "[crabcc-agents/start] ANTHROPIC_API_KEY ← host (${#key} chars, see /tmp/crabcc-key-extract.log)"
  else
    cat /tmp/crabcc-key-extract.log >&2 || true
    echo "[crabcc-agents/start] WARN: ANTHROPIC_API_KEY not propagated — Anthropic models in LiteLLM will fail at request time" >&2
  fi
fi

# Forward to docker-compose / Taskfile. The Taskfile's `up` target
# already calls `litellm:check-build-run` first, so this single
# invocation = key extraction + LiteLLM stack up + agents stack up.
cd "$crate_dir"
exec task up
