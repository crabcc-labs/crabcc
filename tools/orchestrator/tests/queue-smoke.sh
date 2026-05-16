#!/usr/bin/env bash
# queue-smoke.sh — end-to-end smoke test for migrate-queue.sh + queue.sh.
#
# Runs against a temporary database; never touches ~/.crabcc/_agents.db.
# Exits 0 on full pass, 1 on any assertion failure.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
QUEUE="$SCRIPT_DIR/../queue.sh"
MIGRATE="$SCRIPT_DIR/../migrate-queue.sh"

# Use a temp file so the db is automatically cleaned up.
export AGENTS_DB
AGENTS_DB="$(mktemp).db"
cleanup() { rm -f "$AGENTS_DB"; }
trap cleanup EXIT

fail=0

assert() {
    local label="$1"
    local condition="$2"   # a bash test expression passed to eval
    if eval "$condition"; then
        echo "PASS  $label"
    else
        echo "FAIL  $label"
        fail=1
    fi
}

assert_eq() {
    local label="$1"
    local got="$2"
    local want="$3"
    if [[ "$got" == "$want" ]]; then
        echo "PASS  $label"
    else
        echo "FAIL  $label  (got='$got' want='$want')"
        fail=1
    fi
}

# ── step 1: migrate ──────────────────────────────────────────────────────────
msg="$("$MIGRATE")"
assert_eq "migrate: prints migrated" "$msg" "migrated"

msg2="$("$MIGRATE")"
assert_eq "migrate: idempotent (already up to date)" "$msg2" "already up to date"

# ── step 2: enqueue ──────────────────────────────────────────────────────────
TASK_ID="$("$QUEUE" enqueue alpha '{"op":"build","ref":"main"}')"
assert "enqueue: returns numeric id" "[[ $TASK_ID =~ ^[0-9]+$ ]]"

# Enqueue a second task for the requeue→re-claim leg later.
TASK_ID2="$("$QUEUE" enqueue alpha '{"op":"test","ref":"main"}')"
assert "enqueue second task: numeric id" "[[ $TASK_ID2 =~ ^[0-9]+$ ]]"

# ── step 3: claim ────────────────────────────────────────────────────────────
CLAIM_JSON="$("$QUEUE" claim alpha)"
assert "claim: non-empty JSON" "[[ -n \"\$CLAIM_JSON\" ]]"

CLAIMED_ID="$(echo "$CLAIM_JSON" | jq -r '.id')"
assert_eq "claim: id matches first enqueued task" "$CLAIMED_ID" "$TASK_ID"

CLAIMED_STATUS="$(sqlite3 "$AGENTS_DB" "SELECT status FROM agent_tasks WHERE id=$TASK_ID;")"
assert_eq "claim: row status is claimed" "$CLAIMED_STATUS" "claimed"

# ── step 4: done ─────────────────────────────────────────────────────────────
"$QUEUE" done "$TASK_ID" '{"exit":0}'
DONE_STATUS="$(sqlite3 "$AGENTS_DB" "SELECT status FROM agent_tasks WHERE id=$TASK_ID;")"
assert_eq "done: row status is done" "$DONE_STATUS" "done"

DONE_RESULT="$(sqlite3 "$AGENTS_DB" "SELECT result FROM agent_tasks WHERE id=$TASK_ID;")"
assert_eq "done: result stored" "$DONE_RESULT" '{"exit":0}'

# ── step 5: list ─────────────────────────────────────────────────────────────
LIST_OUT="$("$QUEUE" list --agent alpha)"
DONE_ROWS="$(echo "$LIST_OUT" | grep -c "done" || true)"
PENDING_ROWS="$(echo "$LIST_OUT" | grep -c "pending" || true)"
assert "list: at least one done row" "[[ $DONE_ROWS -ge 1 ]]"
assert "list: at least one pending row" "[[ $PENDING_ROWS -ge 1 ]]"

# ── step 6: requeue ──────────────────────────────────────────────────────────
"$QUEUE" requeue "$TASK_ID"
REQUEUED_STATUS="$(sqlite3 "$AGENTS_DB" "SELECT status FROM agent_tasks WHERE id=$TASK_ID;")"
assert_eq "requeue: status back to pending" "$REQUEUED_STATUS" "pending"

# claimed_at / result / error must be cleared.
CLEARED="$(sqlite3 "$AGENTS_DB" \
    "SELECT claimed_at IS NULL AND result IS NULL AND error IS NULL FROM agent_tasks WHERE id=$TASK_ID;")"
assert_eq "requeue: claimed_at/result/error cleared" "$CLEARED" "1"

# ── step 7: claim again after requeue ────────────────────────────────────────
CLAIM2_JSON="$("$QUEUE" claim alpha)"
CLAIMED2_ID="$(echo "$CLAIM2_JSON" | jq -r '.id')"
# The requeued row (TASK_ID) has a lower id than TASK_ID2, so it should be
# claimed first again (FIFO on id).
assert_eq "claim after requeue: oldest pending wins (FIFO)" "$CLAIMED2_ID" "$TASK_ID"

# ── step 8: fail ─────────────────────────────────────────────────────────────
"$QUEUE" fail "$TASK_ID" "compilation error on line 42"
FAILED_STATUS="$(sqlite3 "$AGENTS_DB" "SELECT status FROM agent_tasks WHERE id=$TASK_ID;")"
assert_eq "fail: status is failed" "$FAILED_STATUS" "failed"

FAIL_ERR="$(sqlite3 "$AGENTS_DB" "SELECT error FROM agent_tasks WHERE id=$TASK_ID;")"
assert_eq "fail: error message stored" "$FAIL_ERR" "compilation error on line 42"

# ── step 9: final list ────────────────────────────────────────────────────────
LIST2_OUT="$("$QUEUE" list --agent alpha)"
FAILED_ROWS="$(echo "$LIST2_OUT" | grep -c "failed" || true)"
assert "final list: failed row visible" "[[ $FAILED_ROWS -ge 1 ]]"

# ── step 10: claim returns exit 2 when nothing pending ───────────────────────
# Claim the remaining pending task (TASK_ID2).
"$QUEUE" claim alpha >/dev/null
# Now nothing should be pending.
"$QUEUE" claim alpha >/dev/null && EMPTY_CLAIM_EXIT=0 || EMPTY_CLAIM_EXIT=$?
assert_eq "claim on empty queue: exits 2" "$EMPTY_CLAIM_EXIT" "2"

# ── result ───────────────────────────────────────────────────────────────────
if [[ $fail -eq 0 ]]; then
    echo ""
    echo "PASS  all steps"
else
    echo ""
    echo "FAIL  one or more steps failed"
fi
exit "$fail"
