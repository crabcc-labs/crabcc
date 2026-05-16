#!/usr/bin/env bash
# check-and-done.sh — validate a task result then mark it done or failed.
#
# Usage:
#   check-and-done.sh <task-id> <result-json-file>
#
# Reads the result from a file to handle large outputs safely.
#
# Heuristics (each scored 0=pass / 1=fail):
#   empty_result            result is {}, null, "", or {"content":""}
#   schema_shaped           top-level keys include properties/type/description/enum
#   latency_floor_violation (completed_at - claimed_at) < CHECK_AND_DONE_LATENCY_FLOOR_SEC
#                           (default 2 seconds)
#
# Environment:
#   CHECK_AND_DONE_LATENCY_FLOOR_SEC  minimum acceptable completion time
#                                     in whole seconds (default: 2)
#
# Aggregate:
#   validator_pass          1 if all checks pass, else 0
#
# On pass:  invokes queue.sh done <task-id> <result> — logs persist_done
# On fail:  invokes queue.sh fail <task-id> "validator: <reason>" — logs persist_fail
#
# Stderr log events (per spec):
#   validate_start  validator_score  validator_pass|validator_fail  persist_done|persist_fail
#
# Exit codes:
#   0  validation completed (regardless of pass/fail)
#   1  internal error (task_id not found, result file missing, etc.)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DB="${AGENTS_DB:-$HOME/.crabcc/_agents.db}"

# ── arg check ──────────────────────────────────────────────────────────────
if [[ $# -lt 2 ]]; then
    echo "usage: check-and-done.sh <task-id> <result-json-file>" >&2
    exit 1
fi

TASK_ID="$1"
RESULT_FILE="$2"

if [[ ! "$TASK_ID" =~ ^[0-9]+$ ]]; then
    echo "check-and-done: task-id must be a positive integer, got: $TASK_ID" >&2
    exit 1
fi

if [[ ! -f "$RESULT_FILE" ]]; then
    echo "check-and-done: result file not found: $RESULT_FILE" >&2
    exit 1
fi

LATENCY_FLOOR_SEC="${CHECK_AND_DONE_LATENCY_FLOOR_SEC:-2}"
if [[ ! "$LATENCY_FLOOR_SEC" =~ ^[0-9]+$ ]]; then
    echo "check-and-done: CHECK_AND_DONE_LATENCY_FLOOR_SEC must be a non-negative integer, got: $LATENCY_FLOOR_SEC" >&2
    exit 1
fi

if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "check-and-done: sqlite3 not found" >&2
    exit 1
fi

echo "validate_start task_id=$TASK_ID file=$RESULT_FILE" >&2

# ── verify task exists ─────────────────────────────────────────────────────
# claimed_at / completed_at are stored as ISO 8601 strings
# (strftime('%Y-%m-%dT%H:%M:%SZ','now')). Convert to epoch seconds inside
# SQLite via strftime('%s', …) so the bash side stays portable across
# GNU/BSD `date` differences.
row="$(sqlite3 "$DB" "
    SELECT
        COALESCE(strftime('%s', claimed_at),  '0')           AS claimed_epoch,
        COALESCE(strftime('%s', completed_at), strftime('%s','now')) AS completed_epoch
    FROM agent_tasks WHERE id='$TASK_ID';" 2>/dev/null)"
if [[ -z "$row" ]]; then
    echo "check-and-done: task_id not found: $TASK_ID" >&2
    exit 1
fi

claimed_at="$(printf '%s' "$row" | cut -d'|' -f1)"
completed_at="$(printf '%s' "$row" | cut -d'|' -f2)"

# ── read result ────────────────────────────────────────────────────────────
result="$(cat "$RESULT_FILE")"

# ── check: empty_result ────────────────────────────────────────────────────
score_empty=0
stripped="$(printf '%s' "$result" | tr -d '[:space:]')"
case "$stripped" in
    "{}"|"null"|"\"\""|"")
        score_empty=1 ;;
    *)
        # {"content":""} style — content value is empty string
        if command -v python3 >/dev/null 2>&1; then
            is_empty="$(python3 -c "
import sys, json
try:
    d = json.loads('''$stripped''')
    keys = list(d.keys()) if isinstance(d, dict) else []
    if keys == ['content'] and d.get('content','x') in ('', None):
        print('1')
    else:
        print('0')
except:
    print('0')
" 2>/dev/null || echo "0")"
            score_empty="$is_empty"
        fi
        ;;
esac

# ── check: schema_shaped ───────────────────────────────────────────────────
score_schema=0
if command -v python3 >/dev/null 2>&1; then
    score_schema="$(python3 -c "
import sys, json
SCHEMA_KEYS = {'properties', 'type', 'description', 'enum'}
try:
    data_str = open('$RESULT_FILE').read()
    d = json.loads(data_str)
    if isinstance(d, dict) and SCHEMA_KEYS & set(d.keys()):
        print('1')
    else:
        print('0')
except:
    print('0')
" 2>/dev/null || echo "0")"
fi

# ── check: latency_floor_violation ────────────────────────────────────────
score_latency=0
if [[ "$claimed_at" =~ ^[0-9]+$ && "$completed_at" =~ ^[0-9]+$ ]]; then
    elapsed=$(( completed_at - claimed_at ))
    if [[ $elapsed -lt $LATENCY_FLOOR_SEC ]]; then
        score_latency=1
    fi
else
    # Non-numeric timestamps: treat as violation (suspiciously incomplete state)
    score_latency=1
fi

# ── aggregate ─────────────────────────────────────────────────────────────
if [[ "$score_empty" -eq 0 && "$score_schema" -eq 0 && "$score_latency" -eq 0 ]]; then
    score_pass=1
else
    score_pass=0
fi

scores_json="{\"empty_result\":$score_empty,\"schema_shaped\":$score_schema,\"latency_floor_violation\":$score_latency,\"validator_pass\":$score_pass}"

echo "validator_score task_id=$TASK_ID scores=$scores_json" >&2

# ── write scores to DB ─────────────────────────────────────────────────────
scores_sql="'$(printf '%s' "$scores_json" | sed "s/'/''/g")'"
sqlite3 "$DB" "PRAGMA journal_mode=WAL;" >/dev/null 2>&1 || true
sqlite3 "$DB" "BEGIN IMMEDIATE;
    UPDATE agent_tasks SET validator_scores=$scores_sql WHERE id='$TASK_ID';
    COMMIT;" 2>/dev/null

# ── persist result based on validation outcome ─────────────────────────────
if [[ "$score_pass" -eq 1 ]]; then
    echo "validator_pass task_id=$TASK_ID" >&2
    "$SCRIPT_DIR/queue.sh" done "$TASK_ID" "$result"
    echo "persist_done task_id=$TASK_ID" >&2
else
    # Build reason string listing failed checks.
    reasons=""
    [[ "$score_empty"   -eq 1 ]] && reasons="${reasons:+$reasons, }empty_result"
    [[ "$score_schema"  -eq 1 ]] && reasons="${reasons:+$reasons, }schema_shaped"
    [[ "$score_latency" -eq 1 ]] && reasons="${reasons:+$reasons, }latency_floor_violation"

    echo "validator_fail task_id=$TASK_ID reasons=$reasons" >&2
    "$SCRIPT_DIR/queue.sh" fail "$TASK_ID" "validator: $reasons"
    echo "persist_fail task_id=$TASK_ID reasons=$reasons" >&2
fi

exit 0
