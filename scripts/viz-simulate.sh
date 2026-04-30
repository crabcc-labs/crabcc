#!/usr/bin/env bash
# viz-simulate.sh — simulate ongoing low-effort agent activity against the
# viz crate folder so the live dashboard at /live has something to render.
#
# Pair with `task viz` (in another shell). Each tick picks a random op +
# symbol from a curated list and writes the result to /dev/null; the
# important side-effect is the entry it adds to ~/.crabcc/usage.log,
# which `/api/activity` tails.
#
# Cadence is intentionally low (~3s between calls) — "low effort" per
# the issue. Ctrl-C to stop.

set -uo pipefail

ROOT="${ROOT:-$(pwd)}"
INTERVAL="${INTERVAL:-3}"
CRABCC="${CRABCC:-crabcc}"

# A handful of crabcc-viz symbols (and a couple from crabcc-core) that
# the indexer is guaranteed to have picked up. Keep the list short so
# repetition is recognizable on the dashboard — the live overlay is
# more compelling when the same node pulses twice in a row.
SYMBOLS=(
  "serve"
  "handle"
  "graph_snapshot"
  "activity_tail"
  "bootstrap_snapshot"
  "memory_recent"
  "ensure_initialized"
  "ensure_bin_dir"
  "agent_path"
  "Config"
  "RunDir"
  "Store"
  "CallGraph"
)

OPS=("sym" "callers" "refs" "outline")

# Pick a random element from $@.
pick() { echo "${@:RANDOM%$#+1:1}"; }

echo "viz-simulate: looping every ${INTERVAL}s against ${ROOT}"
echo "viz-simulate: open http://127.0.0.1:7878/live in another window"
echo

while :; do
  op=$(pick "${OPS[@]}")
  sym=$(pick "${SYMBOLS[@]}")

  case "$op" in
    sym)
      "$CRABCC" --root "$ROOT" sym "$sym" >/dev/null 2>&1 || true
      ;;
    callers)
      "$CRABCC" --root "$ROOT" callers "$sym" --count >/dev/null 2>&1 || true
      ;;
    refs)
      "$CRABCC" --root "$ROOT" refs "$sym" --count >/dev/null 2>&1 || true
      ;;
    outline)
      # outline takes a file, not a symbol — pick a viz file. Avoid
      # `shuf` (Linux-only); use awk with $RANDOM for portability so
      # the simulator runs cleanly on macOS too.
      mapfile -t files < <(find crates/crabcc-viz/src -name '*.rs' 2>/dev/null) || files=()
      if [ "${#files[@]}" -gt 0 ]; then
        file="${files[RANDOM % ${#files[@]}]}"
        "$CRABCC" --root "$ROOT" outline "$file" >/dev/null 2>&1 || true
      fi
      ;;
  esac

  ts=$(date +%H:%M:%S)
  printf "  %s  %-7s  %s\n" "$ts" "$op" "$sym"
  sleep "$INTERVAL"
done
