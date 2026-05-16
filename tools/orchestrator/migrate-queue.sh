#!/usr/bin/env bash
# migrate-queue.sh — create (or upgrade) ~/.crabcc/_agents.db with the
# agent_tasks schema. Safe to re-run: all DDL uses CREATE TABLE IF NOT EXISTS.
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

mkdir -p "$(dirname "$AGENTS_DB")"

# Check whether the schema is already present before touching the file.
already_up_to_date() {
    [[ -f "$AGENTS_DB" ]] || return 1
    local count
    count="$(sqlite3 "$AGENTS_DB" \
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='agent_tasks';" 2>/dev/null)" || return 1
    [[ "$count" -ge 1 ]]
}

if already_up_to_date; then
    echo "already up to date"
    exit 0
fi

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

echo "migrated"
