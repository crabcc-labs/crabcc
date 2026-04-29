#!/usr/bin/env bash
# crabcc benchmark — token-cost A/B between raw tools and crabcc.
#
# For each task in tasks.json, runs `claude -p` twice:
#   - raw:    only grep/find/Read are allowed
#   - crabcc: the crabcc CLI is described in the system prompt and on PATH
#
# Captures the JSON transcript per run; report.py turns it into a savings table.
#
# Usage:
#   bench/run.sh <fixture-repo>
#   SKIP_CACHED=1 bench/run.sh <fixture-repo>   # reuse existing per-(task,mode) results
#
# Requires: claude (on PATH), jq, python3, an indexed fixture repo
#           (run `crabcc index --root <repo>` once before benchmarking).

set -euo pipefail

# Ensure cargo-installed binaries (crabcc) are visible to subprocesses.
export PATH="$HOME/.cargo/bin:$PATH"

FIXTURE=${1:?"usage: bench/run.sh <fixture-repo>"}
HERE=$(cd "$(dirname "$0")" && pwd)
RESULTS=$HERE/results
mkdir -p "$RESULTS"

if ! command -v claude >/dev/null 2>&1; then
  echo "error: 'claude' CLI not on PATH" >&2; exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "error: 'jq' not on PATH" >&2; exit 2
fi
if ! command -v crabcc >/dev/null 2>&1; then
  echo "warn: 'crabcc' not on PATH — the crabcc-mode runs will fall back to grep" >&2
fi

PREFACE_RAW=$'You are doing a code-lookup task. Use ONLY the Read, Grep, and Glob tools.\nDo NOT call any other CLI binaries via Bash. Be concise — do not explain.\n\nTask: '

PREFACE_CRABCC=$'You are doing a code-lookup task in a repo that has been indexed with the\n`crabcc` CLI. Prefer crabcc over grep/Read where it fits:\n  crabcc sym <name>      — find a symbol definition (returns JSON)\n  crabcc refs <name>     — find every identifier reference (JSON)\n  crabcc callers <name>  — find call sites (JSON)\n  crabcc outline <file>  — top-level symbols of a file (JSON)\nThe DB lives at .crabcc/index.db. Run crabcc via Bash. Be concise — do not explain.\n\nTask: '

# Ensure a fresh index exists.
if [[ ! -f "$FIXTURE/.crabcc/index.db" ]]; then
  echo "fixture repo has no .crabcc/ — indexing now"
  ( cd "$FIXTURE" && crabcc index >/dev/null )
fi

run_task() {
  local id=$1 prompt=$2 mode=$3 preface=$4
  local out=$RESULTS/${id}-${mode}.json
  if [[ -f "$out" && "${SKIP_CACHED:-0}" == "1" ]]; then
    echo "  [cached] $id $mode"; return
  fi
  local full="${preface}${prompt}"
  echo "  [$mode] $id"
  # `claude -p` reads from stdin; without /dev/null it would consume the
  # outer loop's task stream and only the first task would run.
  ( cd "$FIXTURE" && claude -p "$full" --output-format json < /dev/null ) > "$out" || {
    echo "    (claude exited non-zero, see $out)"; return
  }
}

count=$(jq 'length' "$HERE/tasks.json")
echo "running $count tasks × 2 modes against $FIXTURE"

# Read all tasks into an array first to avoid stdin contention with claude.
mapfile -t TASKS < <(jq -c '.[]' "$HERE/tasks.json")
for task in "${TASKS[@]}"; do
  id=$(jq -r '.id' <<<"$task")
  prompt=$(jq -r '.prompt' <<<"$task")
  run_task "$id" "$prompt" raw    "$PREFACE_RAW"
  run_task "$id" "$prompt" crabcc "$PREFACE_CRABCC"
done

echo
python3 "$HERE/report.py" "$RESULTS"
