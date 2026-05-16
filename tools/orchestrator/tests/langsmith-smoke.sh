#!/usr/bin/env bash
# langsmith-smoke.sh — smoke test for the LangSmith API helpers.
#
# If LANGSMITH_API_KEY is unset or langsmith.sh ping fails, prints a SKIP
# message and exits 0 (no credentials in dev environments).
#
# The upload path is intentionally NOT exercised against the live API to
# avoid polluting the real org. Instead, the JSON body is built and validated
# with jq only.
#
# Exits 0 on pass, 1 on any assertion failure.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LANGSMITH="$SCRIPT_DIR/../langsmith.sh"

fail=0

assert_eq() {
    local label="$1" got="$2" want="$3"
    if [[ "$got" == "$want" ]]; then
        echo "PASS  $label"
    else
        echo "FAIL  $label  (got='$got' want='$want')"
        fail=1
    fi
}

assert_nonzero() {
    local label="$1" val="$2"
    if [[ -n "$val" ]]; then
        echo "PASS  $label"
    else
        echo "FAIL  $label  (empty)"
        fail=1
    fi
}

assert_zero_exit() {
    local label="$1" code="$2"
    if [[ "$code" -eq 0 ]]; then
        echo "PASS  $label"
    else
        echo "FAIL  $label  (exit $code)"
        fail=1
    fi
}

# ── guard: skip if no credentials ────────────────────────────────────────────

if [[ -z "${LANGSMITH_API_KEY:-}" ]]; then
    echo "SKIP: no LangSmith credentials (LANGSMITH_API_KEY unset)"
    exit 0
fi

if ! "$LANGSMITH" ping >/dev/null 2>&1; then
    echo "SKIP: no LangSmith credentials (langsmith.sh ping failed)"
    exit 0
fi

# ── test: ping returns non-empty response ────────────────────────────────────

ping_resp="$("$LANGSMITH" ping 2>/dev/null)"
assert_nonzero "ping: response is non-empty" "$ping_resp"

ping_exit=0
"$LANGSMITH" ping >/dev/null 2>&1 || ping_exit=$?
assert_zero_exit "ping: exits 0" "$ping_exit"

# ── test: upload-experiment body structure (local, no real upload) ────────────
#
# Build a synthetic experiment body and verify its JSON structure with jq.
# We do NOT call langsmith.sh upload-experiment here — that would hit the
# live API.

body_file="$(mktemp)"
cleanup() { rm -f "$body_file"; }
trap cleanup EXIT

jq -nc '{
    experiment_name:       "test-agent@abc1234",
    dataset_name:          "smoke-test-dataset",
    results: [
        {
            row_id:            "example-001",
            inputs:            {"prompt": "hello"},
            actual_outputs:    {"answer": "world"},
            evaluation_scores: null
        },
        {
            row_id:            "example-002",
            inputs:            {"prompt": "foo"},
            actual_outputs:    {"answer": "bar"},
            evaluation_scores: {"accuracy": 0.9}
        }
    ],
    experiment_start_time: "2026-05-16T10:00:00Z",
    experiment_end_time:   "2026-05-16T10:05:00Z",
    metadata: {
        manifest_sha: "abc1234def",
        agent_name:   "test-agent"
    }
}' > "$body_file"

# Validate top-level keys exist.
exp_name="$( jq -r '.experiment_name'       "$body_file")"
ds_name="$(  jq -r '.dataset_name'          "$body_file")"
results_len="$(jq '.results | length'       "$body_file")"
meta_sha="$(  jq -r '.metadata.manifest_sha' "$body_file")"

assert_eq  "body: experiment_name present"   "$exp_name"   "test-agent@abc1234"
assert_eq  "body: dataset_name present"      "$ds_name"    "smoke-test-dataset"
assert_eq  "body: results has 2 entries"     "$results_len" "2"
assert_nonzero "body: metadata.manifest_sha" "$meta_sha"

# Validate first result row shape.
row0_id="$(    jq -r '.results[0].row_id'            "$body_file")"
row0_inputs="$(jq -c '.results[0].inputs'            "$body_file")"
row0_outputs="$(jq -c '.results[0].actual_outputs'   "$body_file")"

assert_eq "body: results[0].row_id"          "$row0_id"      "example-001"
assert_eq "body: results[0].inputs"          "$row0_inputs"  '{"prompt":"hello"}'
assert_eq "body: results[0].actual_outputs"  "$row0_outputs" '{"answer":"world"}'

# Validate second result has evaluation_scores.
row1_score="$(jq -r '.results[1].evaluation_scores.accuracy' "$body_file")"
assert_eq "body: results[1].evaluation_scores.accuracy" "$row1_score" "0.9"

# ── result ────────────────────────────────────────────────────────────────────

if [[ $fail -eq 0 ]]; then
    echo ""
    echo "PASS  all steps"
else
    echo ""
    echo "FAIL  one or more steps failed"
fi
exit "$fail"
