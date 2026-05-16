#!/usr/bin/env bash
# check-and-done-smoke.sh — eight assertions against check-and-done.sh + queue.sh
#
# Uses a temp DB isolated from any real orchestrator state.
# Prints PASS/FAIL per assertion. Exits 0 on all pass, 1 on any failure.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORCH_DIR="$SCRIPT_DIR/.."

TMPDIR_WORK="$(mktemp -d)"
export AGENTS_DB="$(mktemp).db"

cleanup() {
    rm -rf "$TMPDIR_WORK"
    rm -f "$AGENTS_DB"
}
trap cleanup EXIT

# Bootstrap schema via migrate-queue.sh
"$ORCH_DIR/migrate-queue.sh" 2>/dev/null

PASS=0
FAIL=0

assert() {
    local label="$1"
    local result="$2"  # "ok" or error description
    if [[ "$result" == "ok" ]]; then
        echo "PASS  $label"
        PASS=$((PASS+1))
    else
        echo "FAIL  $label — $result"
        FAIL=$((FAIL+1))
    fi
}

# ── Helper: enqueue a task and set claimed_at + completed_at manually ──────
# Usage: enqueue_with_timestamps <agent> <payload> <claimed_at> <completed_at> [--wave-id X] [--example-id Y]
enqueue_with_timestamps() {
    local agent="$1"
    local payload="$2"
    local claimed="$3"
    local completed="$4"
    shift 4
    local task_id
    task_id="$("$ORCH_DIR/queue.sh" enqueue "$agent" "$payload" "$@")"
    # Set claimed_at and completed_at as ISO 8601 (matches production
    # writers in queue.sh). The 'unixepoch' modifier converts epoch ints
    # → ISO inside SQLite, so the test stays portable across GNU/BSD date.
    sqlite3 "$AGENTS_DB" "UPDATE agent_tasks SET
        claimed_at   = strftime('%Y-%m-%dT%H:%M:%SZ', $claimed,   'unixepoch'),
        completed_at = strftime('%Y-%m-%dT%H:%M:%SZ', $completed, 'unixepoch')
     WHERE id='$task_id';"
    echo "$task_id"
}

result_file="$TMPDIR_WORK/result.json"

# ─────────────────────────────────────────────────────────────────────────────
# Assertion 1: Empty result {} → fails with empty_result=1
# ─────────────────────────────────────────────────────────────────────────────
{
    task_id="$(enqueue_with_timestamps "agent-a" '{"job":"test1"}' 1000 1010)"
    printf '{}' > "$result_file"
    "$ORCH_DIR/check-and-done.sh" "$task_id" "$result_file" >/dev/null 2>&1 || true
    scores="$(sqlite3 "$AGENTS_DB" "SELECT validator_scores FROM agent_tasks WHERE id='$task_id';")"
    empty_val="$(printf '%s' "$scores" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('empty_result','?'))" 2>/dev/null || echo "?")"
    status="$(sqlite3 "$AGENTS_DB" "SELECT status FROM agent_tasks WHERE id='$task_id';")"
    if [[ "$empty_val" == "1" && "$status" == "failed" ]]; then
        assert "1: empty {} → empty_result=1, status=failed" "ok"
    else
        assert "1: empty {} → empty_result=1, status=failed" "got empty_result=$empty_val status=$status"
    fi
}

# ─────────────────────────────────────────────────────────────────────────────
# Assertion 2: Schema-shaped result → fails with schema_shaped=1
# ─────────────────────────────────────────────────────────────────────────────
{
    task_id="$(enqueue_with_timestamps "agent-a" '{"job":"test2"}' 1000 1010)"
    printf '{"properties":{"foo":{"type":"string"}},"type":"object"}' > "$result_file"
    "$ORCH_DIR/check-and-done.sh" "$task_id" "$result_file" >/dev/null 2>&1 || true
    scores="$(sqlite3 "$AGENTS_DB" "SELECT validator_scores FROM agent_tasks WHERE id='$task_id';")"
    schema_val="$(printf '%s' "$scores" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('schema_shaped','?'))" 2>/dev/null || echo "?")"
    status="$(sqlite3 "$AGENTS_DB" "SELECT status FROM agent_tasks WHERE id='$task_id';")"
    if [[ "$schema_val" == "1" && "$status" == "failed" ]]; then
        assert "2: schema-shaped → schema_shaped=1, status=failed" "ok"
    else
        assert "2: schema-shaped → schema_shaped=1, status=failed" "got schema_shaped=$schema_val status=$status"
    fi
}

# ─────────────────────────────────────────────────────────────────────────────
# Assertion 3: Fast completion (claimed_at == completed_at) → latency_floor_violation=1
# ─────────────────────────────────────────────────────────────────────────────
{
    now="$(date +%s)"
    task_id="$(enqueue_with_timestamps "agent-a" '{"job":"test3"}' "$now" "$now")"
    printf '{"answer":"real content that is not empty or schema shaped"}' > "$result_file"
    "$ORCH_DIR/check-and-done.sh" "$task_id" "$result_file" >/dev/null 2>&1 || true
    scores="$(sqlite3 "$AGENTS_DB" "SELECT validator_scores FROM agent_tasks WHERE id='$task_id';")"
    latency_val="$(printf '%s' "$scores" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('latency_floor_violation','?'))" 2>/dev/null || echo "?")"
    status="$(sqlite3 "$AGENTS_DB" "SELECT status FROM agent_tasks WHERE id='$task_id';")"
    if [[ "$latency_val" == "1" && "$status" == "failed" ]]; then
        assert "3: fast completion → latency_floor_violation=1, status=failed" "ok"
    else
        assert "3: fast completion → latency_floor_violation=1, status=failed" "got latency_floor_violation=$latency_val status=$status"
    fi
}

# ─────────────────────────────────────────────────────────────────────────────
# Assertion 4: Good result + slow enough → passes, status=done, scores populated
# ─────────────────────────────────────────────────────────────────────────────
{
    now="$(date +%s)"
    claimed=$((now - 10))
    task_id="$(enqueue_with_timestamps "agent-a" '{"job":"test4"}' "$claimed" "$now")"
    printf '{"answer":"this is a real non-empty non-schema result"}' > "$result_file"
    "$ORCH_DIR/check-and-done.sh" "$task_id" "$result_file" >/dev/null 2>&1 || true
    status="$(sqlite3 "$AGENTS_DB" "SELECT status FROM agent_tasks WHERE id='$task_id';")"
    scores="$(sqlite3 "$AGENTS_DB" "SELECT validator_scores FROM agent_tasks WHERE id='$task_id';")"
    pass_val="$(printf '%s' "$scores" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('validator_pass','?'))" 2>/dev/null || echo "?")"
    if [[ "$status" == "done" && "$pass_val" == "1" ]]; then
        assert "4: good result → status=done, validator_pass=1" "ok"
    else
        assert "4: good result → status=done, validator_pass=1" "got status=$status pass_val=$pass_val"
    fi
}

# ─────────────────────────────────────────────────────────────────────────────
# Assertion 5: Bad result → status=failed, error mentions the check that failed
# ─────────────────────────────────────────────────────────────────────────────
{
    task_id="$(enqueue_with_timestamps "agent-a" '{"job":"test5"}' 1000 1010)"
    printf 'null' > "$result_file"
    "$ORCH_DIR/check-and-done.sh" "$task_id" "$result_file" >/dev/null 2>&1 || true
    status="$(sqlite3 "$AGENTS_DB" "SELECT status FROM agent_tasks WHERE id='$task_id';")"
    error_msg="$(sqlite3 "$AGENTS_DB" "SELECT error FROM agent_tasks WHERE id='$task_id';")"
    if [[ "$status" == "failed" && "$error_msg" == *"empty_result"* ]]; then
        assert "5: bad result → status=failed, error mentions failed check" "ok"
    else
        assert "5: bad result → status=failed, error mentions failed check" "got status=$status error=$error_msg"
    fi
}

# ─────────────────────────────────────────────────────────────────────────────
# Assertion 6: validator_scores JSON parseable with all 4 required keys
# ─────────────────────────────────────────────────────────────────────────────
{
    now="$(date +%s)"
    claimed=$((now - 10))
    task_id="$(enqueue_with_timestamps "agent-a" '{"job":"test6"}' "$claimed" "$now")"
    printf '{"answer":"good result for key check"}' > "$result_file"
    "$ORCH_DIR/check-and-done.sh" "$task_id" "$result_file" >/dev/null 2>&1 || true
    scores="$(sqlite3 "$AGENTS_DB" "SELECT validator_scores FROM agent_tasks WHERE id='$task_id';")"
    keys_ok="$(printf '%s' "$scores" | python3 -c "
import sys, json
d = json.load(sys.stdin)
required = {'empty_result','schema_shaped','latency_floor_violation','validator_pass'}
print('ok' if required.issubset(d.keys()) else 'missing:' + str(required - d.keys()))
" 2>/dev/null || echo "parse_error")"
    if [[ "$keys_ok" == "ok" ]]; then
        assert "6: validator_scores has all 4 required keys" "ok"
    else
        assert "6: validator_scores has all 4 required keys" "got $keys_ok from '$scores'"
    fi
}

# ─────────────────────────────────────────────────────────────────────────────
# Assertion 7: Non-existent (but well-formed) task_id → exit 1 + 'not found'
# ─────────────────────────────────────────────────────────────────────────────
{
    printf '{"answer":"irrelevant"}' > "$result_file"
    # 999999 is a well-formed (numeric) task id that no enqueue produced.
    err_out="$("$ORCH_DIR/check-and-done.sh" 999999 "$result_file" 2>&1 || true)"
    exit_code=0
    "$ORCH_DIR/check-and-done.sh" 999999 "$result_file" >/dev/null 2>/dev/null || exit_code=$?
    if [[ "$exit_code" -eq 1 && "$err_out" == *"not found"* ]]; then
        assert "7: nonexistent task_id → exit 1 + 'not found'" "ok"
    else
        assert "7: nonexistent task_id → exit 1 + 'not found'" "got exit=$exit_code msg='$err_out'"
    fi
}

# ─────────────────────────────────────────────────────────────────────────────
# Assertion 5b: Malformed result JSON that slips past heuristics → row must
# end as 'failed' (queue.sh done rejects non-JSON; check-and-done must
# downgrade rather than leave the row stuck as 'claimed').
# ─────────────────────────────────────────────────────────────────────────────
{
    now="$(date +%s)"
    claimed=$((now - 10))
    task_id="$(enqueue_with_timestamps "agent-a" '{"job":"test5b"}' "$claimed" "$now")"
    # Plain text — not valid JSON. Won't trip empty/schema heuristics (they
    # silently return 0 on parse failure) but queue.sh done WILL reject it.
    printf 'plain text not json at all' > "$result_file"
    "$ORCH_DIR/check-and-done.sh" "$task_id" "$result_file" >/dev/null 2>&1 || true
    status="$(sqlite3 "$AGENTS_DB" "SELECT status FROM agent_tasks WHERE id='$task_id';")"
    err="$(sqlite3 "$AGENTS_DB" "SELECT error FROM agent_tasks WHERE id='$task_id';")"
    if [[ "$status" == "failed" && "$err" == *"queue.sh done rejected"* ]]; then
        assert "5b: malformed JSON → row downgraded to failed, not stuck claimed" "ok"
    else
        assert "5b: malformed JSON → row downgraded to failed, not stuck claimed" "got status=$status err='$err'"
    fi
}

# ─────────────────────────────────────────────────────────────────────────────
# Assertion 7b: Malformed (non-numeric) task_id → exit 1 with format error
# ─────────────────────────────────────────────────────────────────────────────
{
    printf '{"answer":"irrelevant"}' > "$result_file"
    err_out="$("$ORCH_DIR/check-and-done.sh" "not-a-number" "$result_file" 2>&1 || true)"
    exit_code=0
    "$ORCH_DIR/check-and-done.sh" "not-a-number" "$result_file" >/dev/null 2>/dev/null || exit_code=$?
    if [[ "$exit_code" -eq 1 && "$err_out" == *"must be a positive integer"* ]]; then
        assert "7b: malformed task_id → exit 1 + format error" "ok"
    else
        assert "7b: malformed task_id → exit 1 + format error" "got exit=$exit_code msg='$err_out'"
    fi
}

# ─────────────────────────────────────────────────────────────────────────────
# Assertion 8: Result file does not exist → exit 1 with clear error
# ─────────────────────────────────────────────────────────────────────────────
{
    task_id="$("$ORCH_DIR/queue.sh" enqueue "agent-a" '{"job":"test8"}')"
    missing_file="$TMPDIR_WORK/does-not-exist.json"
    err_out="$("$ORCH_DIR/check-and-done.sh" "$task_id" "$missing_file" 2>&1 || true)"
    exit_code=0
    "$ORCH_DIR/check-and-done.sh" "$task_id" "$missing_file" >/dev/null 2>/dev/null || exit_code=$?
    if [[ "$exit_code" -eq 1 && "$err_out" == *"not found"* ]]; then
        assert "8: missing result file → exit 1 + 'not found'" "ok"
    else
        assert "8: missing result file → exit 1 + 'not found'" "got exit=$exit_code msg='$err_out'"
    fi
}

# ─────────────────────────────────────────────────────────────────────────────
echo
echo "Results: $PASS passed, $FAIL failed"
[[ $FAIL -eq 0 ]]
