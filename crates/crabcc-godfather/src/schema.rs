//! Schema migrations for the `_crab_*` tables. Idempotent —
//! `Godfather::open` calls [`apply`] every time, and every statement
//! is a `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS`.
//!
//! ## Adding a column
//!
//! Schema is **additive**. Never `DROP COLUMN`. Pattern: add a new
//! `ALTER TABLE ... ADD COLUMN ... NULL` statement guarded by a
//! `pragma_table_info` check, mirroring how
//! `crabcc-core::store::Store::open` handles its idempotent ALTERs.
//!
//! ## Tables
//!
//! | Table | Purpose | Cardinality |
//! |---|---|---|
//! | `_crab_metadata` | Install fingerprint + schema_version | 1 row, KV-shaped |
//! | `_crab_host` | OS / arch / capacity / hashed identifiers | 1 row, refreshed each boot |
//! | `_crab_session` | One row per app boot (viz / desktop / agent / cli) | many |
//! | `_crab_event` | Typed event log (info / warn / error / crash / debug) | many |
//! | `_crab_resource_sample` | RSS / CPU samples linked to a session | many |
//! | `_crab_crash` | Crash event detail — exit code / signal / log tail | 0..N per session |

use anyhow::Result;
use rusqlite::Connection;

/// Bumped only when migrations land — used by future tooling
/// (`crabcc-godfather migrate`) to gate behaviour on schema age.
/// v1 → initial tables.
/// v2 → `_crab_event.severity_int` INTEGER column + index (#488).
pub const SCHEMA_VERSION: i64 = 2;

pub fn apply(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        // WAL keeps the embedded library writers (cli, desktop, viz)
        // and the standalone supervisor (`crabcc-godfather watch`)
        // from blocking each other on the same DB file.
        "PRAGMA journal_mode = WAL;\n\
         PRAGMA synchronous  = NORMAL;\n\
         CREATE TABLE IF NOT EXISTS _crab_metadata (\n\
            key   TEXT PRIMARY KEY,\n\
            value TEXT NOT NULL\n\
         );\n\
         CREATE TABLE IF NOT EXISTS _crab_host (\n\
            id                INTEGER PRIMARY KEY CHECK (id = 1),\n\
            os                TEXT NOT NULL,\n\
            os_version        TEXT NOT NULL,\n\
            arch              TEXT NOT NULL,\n\
            cpu_count         INTEGER NOT NULL,\n\
            total_memory_mb   INTEGER NOT NULL,\n\
            hostname_hash     TEXT NOT NULL,\n\
            machine_id_hash   TEXT NOT NULL,\n\
            updated_at        INTEGER NOT NULL\n\
         );\n\
         CREATE TABLE IF NOT EXISTS _crab_session (\n\
            id           TEXT PRIMARY KEY,\n\
            app          TEXT NOT NULL,\n\
            version      TEXT NOT NULL,\n\
            pid          INTEGER NOT NULL,\n\
            started_at   INTEGER NOT NULL,\n\
            ended_at     INTEGER,\n\
            exit_code    INTEGER,\n\
            exit_signal  INTEGER\n\
         );\n\
         CREATE INDEX IF NOT EXISTS idx_session_app_started \
            ON _crab_session(app, started_at DESC);\n\
         CREATE INDEX IF NOT EXISTS idx_session_active \
            ON _crab_session(ended_at) WHERE ended_at IS NULL;\n\
         CREATE TABLE IF NOT EXISTS _crab_event (\n\
            id          INTEGER PRIMARY KEY AUTOINCREMENT,\n\
            ts          INTEGER NOT NULL,\n\
            session_id  TEXT,\n\
            severity    TEXT NOT NULL,  -- 'debug'|'info'|'warn'|'error'|'crash'\n\
            source      TEXT NOT NULL,  -- 'viz'|'desktop'|'agent'|'cli'|...\n\
            category    TEXT NOT NULL,\n\
            message     TEXT NOT NULL,\n\
            payload     TEXT             -- optional JSON blob\n\
         );\n\
         CREATE INDEX IF NOT EXISTS idx_event_ts ON _crab_event(ts DESC);\n\
         CREATE INDEX IF NOT EXISTS idx_event_session ON _crab_event(session_id);\n\
         CREATE INDEX IF NOT EXISTS idx_event_severity ON _crab_event(severity, ts DESC);\n\
         CREATE TABLE IF NOT EXISTS _crab_resource_sample (\n\
            id          INTEGER PRIMARY KEY AUTOINCREMENT,\n\
            session_id  TEXT NOT NULL,\n\
            ts          INTEGER NOT NULL,\n\
            rss_mb      INTEGER NOT NULL,\n\
            cpu_pct     REAL    NOT NULL,\n\
            vsize_mb    INTEGER NOT NULL\n\
         );\n\
         CREATE INDEX IF NOT EXISTS idx_resource_session_ts \
            ON _crab_resource_sample(session_id, ts DESC);\n\
         CREATE TABLE IF NOT EXISTS _crab_crash (\n\
            id              INTEGER PRIMARY KEY AUTOINCREMENT,\n\
            session_id      TEXT NOT NULL,\n\
            ts              INTEGER NOT NULL,\n\
            exit_code       INTEGER,\n\
            exit_signal     INTEGER,\n\
            log_tail        TEXT,\n\
            report_path     TEXT,\n\
            gh_issue_url    TEXT\n\
         );\n\
         CREATE INDEX IF NOT EXISTS idx_crash_session ON _crab_crash(session_id);\n\
         CREATE INDEX IF NOT EXISTS idx_crash_ts      ON _crab_crash(ts DESC);",
    )?;

    // Stamp `schema_version` on every open. Idempotent INSERT-or-IGNORE
    // followed by an UPDATE so a future bump always lands without
    // dropping pre-existing keys (install_time, etc.).
    conn.execute(
        "INSERT INTO _crab_metadata(key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![SCHEMA_VERSION.to_string()],
    )?;

    // Additive migration #488 — add `severity_int` so new event rows
    // store severity as a 1-byte INTEGER instead of a 5-7 byte TEXT.
    // The TEXT column is kept for back-compat reads of pre-migration
    // rows; new writes set `severity_int` and leave `severity` NULL,
    // which SQLite stores as a single type tag (~1 byte).
    add_column_if_missing(conn, "_crab_event", "severity_int", "INTEGER")?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_event_severity_int \
         ON _crab_event(severity_int, ts DESC);",
    )?;
    // One-time backfill — idempotent because the WHERE clause skips
    // any row that already has `severity_int`. Cheap on small tables;
    // becomes a no-op once every row has been seen.
    conn.execute(
        "UPDATE _crab_event
         SET severity_int = CASE severity
            WHEN 'debug' THEN 0
            WHEN 'info'  THEN 1
            WHEN 'warn'  THEN 2
            WHEN 'error' THEN 3
            WHEN 'crash' THEN 4
            ELSE NULL
         END
         WHERE severity_int IS NULL AND severity IS NOT NULL",
        [],
    )?;

    Ok(())
}

/// Idempotent ALTER — checks `pragma_table_info` before adding so a
/// re-open after the first migration doesn't re-execute the ALTER
/// (which would error). Mirrors the pattern in
/// `crabcc-core::store::Store::open`.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    type_decl: &str,
) -> Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        rusqlite::params![table, column],
        |row| row.get(0),
    )?;
    if exists == 0 {
        // No params binding — table / column / type_decl are
        // identifiers, not values.
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {type_decl};"
        ))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open_inmem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        apply(&c).unwrap();
        c
    }

    /// Calling `apply` twice on the same connection must be a no-op
    /// (schema is supposed to survive every `Godfather::open`, including
    /// re-opens of a long-lived DB). If a future migration ever breaks
    /// this, the lazy-prune-on-open path would crash on the second
    /// boot — exactly the regression this catches.
    #[test]
    fn apply_is_idempotent() {
        let c = open_inmem();
        apply(&c).unwrap();
        apply(&c).unwrap();
        // Schema version row still exactly one entry.
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM _crab_metadata WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn schema_version_row_matches_constant() {
        let c = open_inmem();
        let v: String = c
            .query_row(
                "SELECT value FROM _crab_metadata WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION.to_string());
    }

    /// Every table the rest of the crate writes to must exist after
    /// `apply` — guards against a future "I'll add the table later"
    /// commit that lands the writer before the migration.
    #[test]
    fn all_expected_tables_exist() {
        let c = open_inmem();
        for table in [
            "_crab_metadata",
            "_crab_host",
            "_crab_session",
            "_crab_event",
            "_crab_resource_sample",
            "_crab_crash",
        ] {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master \
                     WHERE type = 'table' AND name = ?1",
                    rusqlite::params![table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {table} missing");
        }
    }

    /// Indexes are how the cleanup module's `WHERE ts < ?` queries
    /// stay sub-millisecond on a multi-thousand-row event log. A
    /// future PR that drops one would silently regress — pin them.
    #[test]
    fn critical_indexes_exist() {
        let c = open_inmem();
        for idx in [
            "idx_session_app_started",
            "idx_session_active",
            "idx_event_ts",
            "idx_event_session",
            "idx_event_severity",
            "idx_event_severity_int",
            "idx_resource_session_ts",
            "idx_crash_session",
            "idx_crash_ts",
        ] {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master \
                     WHERE type = 'index' AND name = ?1",
                    rusqlite::params![idx],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "index {idx} missing");
        }
    }
}
