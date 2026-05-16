#!/usr/bin/env bash
# queue.sh — SQLite-backed agent task queue CLI.
#
# Usage:
#   queue.sh enqueue  <agent> <payload-json> [manifest_sha] [--wave-id <id>] [--example-id <id>]
#   queue.sh claim    <agent>
#   queue.sh done     <task-id> <result-json>
#   queue.sh fail     <task-id> <error-msg>
#   queue.sh requeue  <task-id>
#   queue.sh status   [task-id]
#   queue.sh list     [--agent X] [--status Y] [--limit N]
#   queue.sh list-by-wave <wave-id>
#
# Environment overrides:
#   AGENTS_DB   path to the SQLite database (default: ~/.crabcc/_agents.db)
#
# Exit codes:
#   0   success
#   1   usage error, missing prerequisite, or SQL error
#   2   claim: nothing pending for the given agent

set -uo pipefail

AGENTS_DB="${AGENTS_DB:-$HOME/.crabcc/_agents.db}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ── prerequisites ────────────────────────────────────────────────────────────

if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "queue.sh: sqlite3 not found on PATH" >&2
    exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "queue.sh: jq not found on PATH" >&2
    exit 1
fi

# ── helpers ──────────────────────────────────────────────────────────────────

# Escape single-quotes for SQL string literals.
sq() { printf '%s' "$1" | sed "s/'/''/g"; }

# Run SQL against the database (plain text output). WAL + FK are always set.
# $1 = SQL string
db() {
    printf 'PRAGMA journal_mode=WAL;\nPRAGMA foreign_keys=ON;\n%s\n' "$1" \
        | sqlite3 "$AGENTS_DB" | grep -v '^wal$' || true
}

# Run SQL and return JSON rows. PRAGMAs are executed first in a plain call
# (suppressed) so that -json mode only sees real result sets.
# $1 = SQL string
db_json() {
    # Set WAL + FK silently first.
    printf 'PRAGMA journal_mode=WAL;\nPRAGMA foreign_keys=ON;\n' \
        | sqlite3 "$AGENTS_DB" >/dev/null 2>&1 || true
    # Now run the real SQL in JSON mode.
    printf '%s\n' "$1" | sqlite3 -json "$AGENTS_DB"
}

# Same but with -column -header output mode.
db_table() {
    printf 'PRAGMA journal_mode=WAL;\nPRAGMA foreign_keys=ON;\n' \
        | sqlite3 "$AGENTS_DB" >/dev/null 2>&1 || true
    printf '%s\n' "$1" | sqlite3 -column -header "$AGENTS_DB"
}

# Ensure the DB, base table, AND v0.1 columns all exist (idempotent).
# Checking only for the table would let a pre-v0.1 DB skip migration —
# then INSERT into wave_id / langsmith_example_id would fail silently
# because db() swallows errors with `|| true`.
ensure_migrated() {
    if [[ ! -f "$AGENTS_DB" ]]; then
        "$SCRIPT_DIR/migrate-queue.sh" >/dev/null
        return
    fi
    # Check both: base table exists AND wave_id column is present.
    # wave_id is the marker of v0.1; if it's missing, run the migration
    # which idempotently adds all three additive columns.
    local has_wave
    has_wave="$(sqlite3 "$AGENTS_DB" \
        "SELECT COUNT(*) FROM pragma_table_info('agent_tasks') WHERE name='wave_id';" \
        2>/dev/null)" || has_wave=0
    if [[ "${has_wave:-0}" -lt 1 ]]; then
        "$SCRIPT_DIR/migrate-queue.sh" >/dev/null
    fi
}

die() { echo "queue.sh: $*" >&2; exit 1; }

# ── subcommands ───────────────────────────────────────────────────────────────

cmd_enqueue() {
    [[ $# -ge 2 ]] || die "usage: enqueue <agent> <payload-json> [manifest_sha] [--wave-id <id>] [--example-id <id>]"
    local agent="$1"
    local payload="$2"
    shift 2

    local manifest_sha=""
    local wave_id=""
    local example_id=""

    # 3rd positional arg (if not a flag) is manifest_sha — preserves the
    # legacy `enqueue <agent> <payload> <manifest_sha>` calling convention.
    if [[ $# -gt 0 && "$1" != --* ]]; then
        manifest_sha="$1"; shift
    fi

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --wave-id)       wave_id="$2";       shift 2 ;;
            --example-id)    example_id="$2";    shift 2 ;;
            --manifest-sha)  manifest_sha="$2";  shift 2 ;;
            *) die "unknown flag: $1" ;;
        esac
    done

    ensure_migrated

    # Validate JSON payload.
    printf '%s' "$payload" | jq -e . >/dev/null 2>&1 \
        || die "payload is not valid JSON"

    local sha_expr  wave_expr  example_expr
    [[ -n "$manifest_sha" ]] && sha_expr="'$(sq "$manifest_sha")'"      || sha_expr=NULL
    [[ -n "$wave_id"      ]] && wave_expr="'$(sq "$wave_id")'"          || wave_expr=NULL
    [[ -n "$example_id"   ]] && example_expr="'$(sq "$example_id")'"    || example_expr=NULL

    local sql
    sql="BEGIN IMMEDIATE;
INSERT INTO agent_tasks(agent, payload, manifest_sha, wave_id, langsmith_example_id)
    VALUES('$(sq "$agent")', '$(sq "$payload")', $sha_expr, $wave_expr, $example_expr);
SELECT last_insert_rowid();
COMMIT;"

    local id
    id="$(db "$sql" | tail -1)"
    echo "$id"
}

cmd_list_by_wave() {
    [[ $# -ge 1 ]] || die "usage: list-by-wave <wave-id>"
    local wave_id="$1"
    ensure_migrated
    db_json "SELECT * FROM agent_tasks WHERE wave_id = '$(sq "$wave_id")' ORDER BY id;"
}

cmd_claim() {
    [[ $# -ge 1 ]] || die "usage: claim <agent>"
    local agent="$1"

    ensure_migrated

    # Atomic claim: one BEGIN IMMEDIATE transaction — UPDATE then SELECT.
    # sqlite3 runs all statements sequentially; no second connection can sneak
    # in between because the write lock is held for the duration.
    # changes() returns the number of rows modified by the last UPDATE; if it
    # is 0 (nothing pending), the SELECT returns an empty result set.
    local sql
    sql="BEGIN IMMEDIATE;
UPDATE agent_tasks
SET    status     = 'claimed',
       claimed_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
WHERE  id = (
    SELECT id FROM agent_tasks
    WHERE  agent  = '$(sq "$agent")'
      AND  status = 'pending'
    ORDER BY id ASC
    LIMIT 1
);
SELECT id, payload, manifest_sha
FROM   agent_tasks
WHERE  changes() > 0
  AND  agent  = '$(sq "$agent")'
  AND  status = 'claimed'
ORDER BY claimed_at DESC, id ASC
LIMIT 1;
COMMIT;"

    local json
    json="$(db_json "$sql" 2>/dev/null || true)"

    # sqlite3 -json returns "" or "[]" when nothing matched.
    if [[ -z "$json" || "$json" == "[]" ]]; then
        exit 2
    fi
    # Unwrap the single-element array.
    printf '%s\n' "$json" | jq -c '.[0]'
}

cmd_done() {
    [[ $# -ge 2 ]] || die "usage: done <task-id> <result-json>"
    local task_id="$1"
    local result="$2"

    [[ "$task_id" =~ ^[0-9]+$ ]] || die "task-id must be an integer"
    printf '%s' "$result" | jq -e . >/dev/null 2>&1 \
        || die "result is not valid JSON"

    ensure_migrated

    local sql
    sql="BEGIN IMMEDIATE;
UPDATE agent_tasks
SET    status       = 'done',
       completed_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'),
       result       = '$(sq "$result")'
WHERE  id = $task_id;
COMMIT;"
    db "$sql"
}

cmd_fail() {
    [[ $# -ge 2 ]] || die "usage: fail <task-id> <error-msg>"
    local task_id="$1"
    local error_msg="$2"

    [[ "$task_id" =~ ^[0-9]+$ ]] || die "task-id must be an integer"

    ensure_migrated

    local sql
    sql="BEGIN IMMEDIATE;
UPDATE agent_tasks
SET    status       = 'failed',
       completed_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'),
       error        = '$(sq "$error_msg")'
WHERE  id = $task_id;
COMMIT;"
    db "$sql"
}

cmd_requeue() {
    [[ $# -ge 1 ]] || die "usage: requeue <task-id>"
    local task_id="$1"

    [[ "$task_id" =~ ^[0-9]+$ ]] || die "task-id must be an integer"

    ensure_migrated

    local sql
    sql="BEGIN IMMEDIATE;
UPDATE agent_tasks
SET    status       = 'pending',
       claimed_at   = NULL,
       completed_at = NULL,
       result       = NULL,
       error        = NULL
WHERE  id = $task_id;
COMMIT;"
    db "$sql"
}

cmd_status() {
    ensure_migrated
    local sql
    if [[ $# -ge 1 && -n "$1" ]]; then
        local task_id="$1"
        [[ "$task_id" =~ ^[0-9]+$ ]] || die "task-id must be an integer"
        sql="SELECT * FROM agent_tasks WHERE id = $task_id;"
    else
        sql="SELECT * FROM agent_tasks ORDER BY id;"
    fi
    db_json "$sql" | jq -c '.[]'
}

cmd_list() {
    ensure_migrated

    local filter_agent=""
    local filter_status=""
    local limit=50

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --agent)  filter_agent="$2";  shift 2 ;;
            --status) filter_status="$2"; shift 2 ;;
            --limit)  limit="$2";         shift 2 ;;
            *) die "unknown flag: $1" ;;
        esac
    done

    [[ "$limit" =~ ^[0-9]+$ ]] || die "--limit must be a positive integer"

    local where="WHERE 1=1"
    [[ -n "$filter_agent"  ]] && where="$where AND agent  = '$(sq "$filter_agent")'"
    [[ -n "$filter_status" ]] && where="$where AND status = '$(sq "$filter_status")'"

    local sql
    sql="SELECT id, agent, status, created_at, claimed_at, completed_at
FROM   agent_tasks
$where
ORDER  BY id
LIMIT  $limit;"
    db_table "$sql"
}

# ── dispatch ──────────────────────────────────────────────────────────────────

[[ $# -ge 1 ]] || { echo "usage: queue.sh <subcommand> [args...]" >&2; exit 1; }
SUBCMD="$1"; shift

case "$SUBCMD" in
    enqueue)      cmd_enqueue       "$@" ;;
    claim)        cmd_claim         "$@" ;;
    done)         cmd_done          "$@" ;;
    fail)         cmd_fail          "$@" ;;
    requeue)      cmd_requeue       "$@" ;;
    status)       cmd_status        "${1:-}" ;;
    list)         cmd_list          "$@" ;;
    list-by-wave) cmd_list_by_wave  "$@" ;;
    *) die "unknown subcommand: $SUBCMD" ;;
esac
