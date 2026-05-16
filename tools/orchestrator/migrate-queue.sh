#!/usr/bin/env bash
# migrate-queue.sh — create (or upgrade) ~/.crabcc/_agents.db with the
# agent_tasks schema. Safe to re-run: all DDL is idempotent (CREATE TABLE IF
# NOT EXISTS for the base table, PRAGMA table_info check before each
# additive ALTER TABLE).
#
# Environment overrides:
#   AGENTS_DB   path to the SQLite database (default: ~/.crabcc/_agents.db)
#
# Exit codes:
#   0   migrated or already up to date
#   1   sqlite3 not found or SQL error

set -uo pipefail

AGENTS_DB="${AGENTS_DB:-$HOME/.crabcc/_agents.db}"

if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "migrate-queue.sh: sqlite3 not found on PATH" >&2
    exit 1
fi

mkdir -p "$(dirname "$AGENTS_DB")" || {
    echo "migrate-queue.sh: failed to create parent directory for $AGENTS_DB" >&2
    exit 1
}

# True iff agent_tasks table is present.
base_table_present() {
    [[ -f "$AGENTS_DB" ]] || return 1
    local count
    count="$(sqlite3 "$AGENTS_DB" \
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='agent_tasks';" 2>/dev/null)" || return 1
    [[ "$count" -ge 1 ]]
}

# True iff $1 is a column on agent_tasks.
has_column() {
    local col="$1"
    sqlite3 "$AGENTS_DB" "PRAGMA table_info(agent_tasks);" 2>/dev/null \
        | awk -F'|' -v c="$col" '$2 == c { found=1 } END { exit !found }'
}

V01_COLUMNS=(wave_id langsmith_example_id validator_scores)

# Decide whether anything needs doing before we touch the DB.
schema_up_to_date() {
    base_table_present || return 1
    local col
    for col in "${V01_COLUMNS[@]}"; do
        has_column "$col" || return 1
    done
    return 0
}

if schema_up_to_date; then
    echo "already up to date"
    exit 0
fi

# ── base table + index (idempotent on second-pass v0.1 upgrade) ──────────────
sqlite3 "$AGENTS_DB" <<'SQL' >/dev/null
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS agent_tasks (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    agent        TEXT    NOT NULL,
    payload      TEXT    NOT NULL,       -- arbitrary JSON
    manifest_sha TEXT,                  -- optional; set by caller
    status       TEXT    NOT NULL DEFAULT 'pending'
                         CHECK (status IN ('pending','claimed','done','failed')),
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    claimed_at   TEXT,
    completed_at TEXT,
    result       TEXT,                  -- JSON result on done
    error        TEXT                   -- error message on failed
);

CREATE INDEX IF NOT EXISTS idx_agent_tasks_agent_status
    ON agent_tasks(agent, status, id);
SQL

# ── additive v0.1 columns ────────────────────────────────────────────────────
for col in "${V01_COLUMNS[@]}"; do
    if has_column "$col"; then
        echo "migrate-queue: column '$col' already present" >&2
    else
        sqlite3 "$AGENTS_DB" "ALTER TABLE agent_tasks ADD COLUMN $col TEXT;"
        echo "migrate-queue: added column '$col'" >&2
    fi
done

sqlite3 "$AGENTS_DB" "CREATE INDEX IF NOT EXISTS idx_agent_tasks_wave ON agent_tasks(wave_id);"

echo "migrated"
