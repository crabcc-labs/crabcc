#!/usr/bin/env bash
# import-dataset.sh — fetch a LangSmith dataset and enqueue its examples for
# an agent.
#
# Usage:
#   import-dataset.sh <dataset-name> <agent>
#
# Stdout: the generated wave_id (e.g. "import-mydata-1716000000")
# Stderr: structured logs per the logging contract.
#
# Environment:
#   LANGSMITH_API_KEY   (required — forwarded to langsmith.sh)
#   LANGSMITH_ENDPOINT  (optional)
#   AGENTS_DB           (optional — forwarded to queue.sh)
#
# The wave_id printed to stdout can be piped directly to upload-experiment.sh
# once all tasks are terminal.

set -uo pipefail

[[ $# -ge 2 ]] || { echo "usage: import-dataset.sh <dataset-name> <agent>" >&2; exit 1; }

DATASET_NAME="$1"
AGENT="$2"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LANGSMITH="$SCRIPT_DIR/langsmith.sh"
QUEUE="$SCRIPT_DIR/queue.sh"

[[ -x "$LANGSMITH" ]] || { echo "import-dataset.sh: langsmith.sh not executable: $LANGSMITH" >&2; exit 1; }
[[ -x "$QUEUE"     ]] || { echo "import-dataset.sh: queue.sh not executable: $QUEUE" >&2; exit 1; }

log() {
    local level="$1"; shift
    local event="$1"; shift
    printf '[import-dataset] %s %s %s' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$level" "$event" >&2
    for kv in "$@"; do printf ' %s' "$kv" >&2; done
    printf '\n' >&2
}

die() { log ERROR fatal msg="$*"; exit 1; }

# ── resolve dataset ───────────────────────────────────────────────────────────

log INFO dataset_fetch_start dataset_name="$DATASET_NAME" agent="$AGENT"

dataset_json="$("$LANGSMITH" get-dataset "$DATASET_NAME")" || {
    log ERROR dataset_fetch_error dataset_name="$DATASET_NAME"
    exit 1
}

DATASET_ID="$(printf '%s\n' "$dataset_json" | jq -r '.id')"
[[ -n "$DATASET_ID" && "$DATASET_ID" != "null" ]] \
    || die "could not extract dataset id from response"

log INFO dataset_resolved dataset_name="$DATASET_NAME" dataset_id="$DATASET_ID"

# ── fetch examples ────────────────────────────────────────────────────────────

examples_json="$("$LANGSMITH" list-examples "$DATASET_ID")" || {
    log ERROR dataset_fetch_error dataset_id="$DATASET_ID"
    exit 1
}

example_count="$(printf '%s\n' "$examples_json" | jq 'length')"
log INFO dataset_fetch_done dataset_name="$DATASET_NAME" example_count="$example_count"

# ── generate wave_id ──────────────────────────────────────────────────────────

# Sanitize dataset name for use in the wave_id (alphanumeric + hyphens only).
# Include the PID + a $RANDOM suffix so two concurrent imports of the same
# dataset in the same second do not collide on wave_id.
safe_name="$(printf '%s' "$DATASET_NAME" | tr -cs 'a-zA-Z0-9-' '-' | tr '[:upper:]' '[:lower:]' | sed 's/^-//;s/-$//')"
WAVE_ID="import-${safe_name}-$(date +%s)-$$-$RANDOM"

log INFO wave_generated wave_id="$WAVE_ID"

# ── enqueue one task per example ─────────────────────────────────────────────

enqueue_ok=0
enqueue_fail=0

while IFS= read -r example; do
    example_id="$(printf '%s\n' "$example" | jq -r '.id // empty')"
    [[ -n "$example_id" ]] || { log WARN skip_example msg="missing id in example row"; continue; }

    inputs="$(printf '%s\n' "$example" | jq -c '.inputs // {}')"
    outputs="$(printf '%s\n' "$example" | jq -c '.outputs // {}')"

    payload="$(jq -nc \
        --arg eid  "$example_id" \
        --arg dname "$DATASET_NAME" \
        --arg wid  "$WAVE_ID" \
        --argjson inp "$inputs" \
        --argjson out "$outputs" \
        '{langsmith_example_id: $eid, dataset_name: $dname, wave_id: $wid, inputs: $inp, expected_outputs: $out}')"

    # Attempt to use --wave-id and --example-id flags if queue.sh supports them.
    # Fall back to embedding them in the payload (which we already do) and log WARN.
    if "$QUEUE" enqueue "$AGENT" "$payload" --wave-id "$WAVE_ID" --example-id "$example_id" >/dev/null 2>/tmp/queue_enqueue_err; then
        log INFO enqueue_ok agent="$AGENT" example_id="$example_id" wave_id="$WAVE_ID"
        enqueue_ok=$((enqueue_ok + 1))
    elif "$QUEUE" enqueue "$AGENT" "$payload" >/dev/null 2>/dev/null; then
        log WARN enqueue_flag_fallback agent="$AGENT" example_id="$example_id" \
            msg="queue.sh does not support --wave-id/--example-id; wave_id embedded in payload"
        enqueue_ok=$((enqueue_ok + 1))
    else
        log ERROR enqueue_fail agent="$AGENT" example_id="$example_id"
        enqueue_fail=$((enqueue_fail + 1))
    fi
done < <(printf '%s\n' "$examples_json" | jq -c '.[]')

log INFO import_done wave_id="$WAVE_ID" enqueue_ok="$enqueue_ok" enqueue_fail="$enqueue_fail"

if [[ $enqueue_fail -gt 0 ]]; then
    log WARN partial_import wave_id="$WAVE_ID" failed="$enqueue_fail"
fi

printf '%s\n' "$WAVE_ID"
