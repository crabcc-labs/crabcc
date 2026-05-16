#!/usr/bin/env bash
# upload-experiment.sh — collect terminal queue rows for a wave and upload
# them as a LangSmith experiment.
#
# Usage:
#   upload-experiment.sh <wave-id>
#
# Stdout: the LangSmith experiment URL (from the API response)
# Stderr: structured logs per the logging contract.
#
# Environment:
#   LANGSMITH_API_KEY   (required — forwarded to langsmith.sh)
#   LANGSMITH_ENDPOINT  (optional)
#   AGENTS_DB           (optional — path to SQLite db, default: ~/.crabcc/_agents.db)

set -uo pipefail

[[ $# -ge 1 ]] || { echo "usage: upload-experiment.sh <wave-id>" >&2; exit 1; }

WAVE_ID="$1"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LANGSMITH="$SCRIPT_DIR/langsmith.sh"
QUEUE="$SCRIPT_DIR/queue.sh"
AGENTS_DB="${AGENTS_DB:-$HOME/.crabcc/_agents.db}"

[[ -x "$LANGSMITH" ]] || { echo "upload-experiment.sh: langsmith.sh not executable: $LANGSMITH" >&2; exit 1; }
[[ -x "$QUEUE"     ]] || { echo "upload-experiment.sh: queue.sh not executable: $QUEUE" >&2; exit 1; }

log() {
    local level="$1"; shift
    local event="$1"; shift
    printf '[upload-experiment] %s %s %s' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$level" "$event" >&2
    for kv in "$@"; do printf ' %s' "$kv" >&2; done
    printf '\n' >&2
}

die() { log ERROR fatal msg="$*"; exit 1; }

# ── collect terminal rows ─────────────────────────────────────────────────────

log INFO wave_collect_start wave_id="$WAVE_ID"

# queue.sh exposes list-by-wave as a first-class subcommand (v0.1 schema
# addition). Returns JSON array of ALL statuses for the wave; filter to
# terminal rows (done/failed) here so the rest of the script only deals
# with rows worth uploading.
rows_json="$("$QUEUE" list-by-wave "$WAVE_ID" 2>/dev/null || true)"
if [[ -n "$rows_json" && "$rows_json" != "[]" && "$rows_json" != "null" ]]; then
    rows_json="$(printf '%s' "$rows_json" | jq -c '[.[] | select(.status == "done" or .status == "failed")]')"
fi

# Normalise: if rows_json is empty or "[]" we have nothing to upload.
if [[ -z "$rows_json" || "$rows_json" == "[]" || "$rows_json" == "null" ]]; then
    # Also try: maybe queue list returned columnar output; try sqlite3 directly.
    if [[ -f "$AGENTS_DB" ]]; then
        rows_json="$(sqlite3 -json "$AGENTS_DB" \
            "SELECT id, agent, payload, manifest_sha, status,
                    created_at, claimed_at, completed_at, result, error
             FROM   agent_tasks
             WHERE  json_extract(payload, '$.wave_id') = '$(printf '%s' "$WAVE_ID" | sed "s/'/''/g")'
               AND  status IN ('done', 'failed')
             ORDER BY id;" 2>/dev/null || echo "[]")"
    fi
fi

[[ -n "$rows_json" && "$rows_json" != "[]" && "$rows_json" != "null" ]] \
    || die "no terminal rows found for wave_id=$WAVE_ID"

row_count="$(printf '%s\n' "$rows_json" | jq 'length')"
log INFO rows_collected wave_id="$WAVE_ID" count="$row_count"

# ── build experiment body ─────────────────────────────────────────────────────

# Extract fields from the first row for experiment-level metadata.
first_row="$(printf '%s\n' "$rows_json" | jq -c '.[0]')"
agent_name="$(printf '%s\n' "$first_row" | jq -r '.agent // "unknown"')"
manifest_sha="$(printf '%s\n' "$first_row" | jq -r '.manifest_sha // ""')"
sha_short="${manifest_sha:0:7}"
dataset_name="$(printf '%s\n' "$first_row" | jq -r '.payload | fromjson | .dataset_name // "unknown"' 2>/dev/null || echo "unknown")"

exp_name="${agent_name}@${sha_short:-unknown}"

# MIN(claimed_at) / MAX(completed_at) across all rows.
exp_start="$(printf '%s\n' "$rows_json" | jq -r '[.[].claimed_at   | select(. != null)] | sort | .[0]  // "unknown"')"
exp_end="$(  printf '%s\n' "$rows_json" | jq -r '[.[].completed_at | select(. != null)] | sort | .[-1] // "unknown"')"

# Build results array: one entry per terminal row.
results_json="$(printf '%s\n' "$rows_json" | jq -c '[.[] | {
    row_id:            (.payload | try fromjson | .langsmith_example_id // (.id | tostring)),
    inputs:            (.payload | try fromjson | .inputs // {}),
    actual_outputs:    (.result  | if . then try fromjson else {} end // {}),
    evaluation_scores: (
        .result
        | if . then try fromjson else null end
        | if . then (.validator_scores // null) else null end
    )
}]')"

body_file="$(mktemp)"

jq -nc \
    --arg   exp_name       "$exp_name" \
    --arg   dataset_name   "$dataset_name" \
    --arg   exp_start      "$exp_start" \
    --arg   exp_end        "$exp_end" \
    --arg   manifest_sha   "$manifest_sha" \
    --arg   agent_name     "$agent_name" \
    --argjson results      "$results_json" \
    '{
        experiment_name:       $exp_name,
        dataset_name:          $dataset_name,
        results:               $results,
        experiment_start_time: $exp_start,
        experiment_end_time:   $exp_end,
        metadata: {
            manifest_sha: $manifest_sha,
            agent_name:   $agent_name
        }
    }' > "$body_file"

cleanup() { rm -f "$body_file"; }
trap cleanup EXIT

# ── upload ────────────────────────────────────────────────────────────────────

log INFO upload_start wave_id="$WAVE_ID" experiment_name="$exp_name" rows="$row_count"

resp="$("$LANGSMITH" upload-experiment "$body_file")" || {
    log ERROR upload_error wave_id="$WAVE_ID"
    exit 1
}

experiment_id="$(printf '%s\n' "$resp" | jq -r '.experiment_id // empty')"
dataset_id="$(  printf '%s\n' "$resp" | jq -r '.dataset_id   // empty')"

log INFO upload_ok wave_id="$WAVE_ID" experiment_id="$experiment_id" dataset_id="$dataset_id"

# Print the experiment URL.  The LangSmith URL pattern for EU:
# https://eu.smith.langchain.com/projects/<experiment_id>
LANGSMITH_BASE_UI="${LANGSMITH_ENDPOINT/api\./}"
LANGSMITH_BASE_UI="${LANGSMITH_BASE_UI%/api}"
# Fallback for non-standard endpoints.
LANGSMITH_BASE_UI="${LANGSMITH_BASE_UI:-https://eu.smith.langchain.com}"

printf '%s/projects/%s\n' "$LANGSMITH_BASE_UI" "$experiment_id"
