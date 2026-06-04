//! Cross-session shell ledger. Mirrors [`crate::read`]'s storage
//! shape but for Bash invocations instead of file reads.
//!
//! The PreToolUse Bash hook calls `crabcc shell record` on every
//! command Claude Code is about to fire. This module owns the
//! UPSERT into `session_shells` (keyed on `(command, cwd,
//! session_id)`, bumping `run_count` on every repeat) and the
//! query surface the loop detector (#5 of the lean-ctx plan)
//! reads from.
//!
//! Why no stdout / exit_code: the hook fires BEFORE the command
//! runs, so neither is available. Observation + loop detection is
//! the goal; output compression is RTK's job (see
//! `~/.claude/RTK.md`).

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// One row's worth of state from `session_shells`. Returned by
/// [`lookup_shell`] for the loop-detector path.
#[derive(Debug, Clone)]
pub struct ShellRecord {
    pub command: String,
    pub cwd: String,
    pub session_id: Option<String>,
    pub last_run_at: i64,
    pub run_count: i64,
}

/// Sentinel stored in the `session_id` column when the caller
/// passes `None`. SQLite's UNIQUE constraint treats NULLs as
/// distinct, so a NULL-session caller would never trigger the
/// UPSERT path — we'd get fresh `run_count=1` rows on every call
/// instead of bumping the count. Round-trip through this sentinel
/// internally; the public API still surfaces `Option<String>`.
const NULL_SESSION_SENTINEL: &str = "";

/// UPSERT a Bash invocation into `session_shells`. Idempotent —
/// repeats on the same `(command, cwd, session_id)` bump
/// `run_count` and refresh `last_run_at`. `Palace::open` runs once
/// up-front so the db file + schema exist before the raw rusqlite
/// call.
pub fn record_shell(
    root: &Path,
    command: &str,
    cwd: &str,
    session_id: Option<&str>,
) -> Result<ShellRecord> {
    let _ = crate::Palace::open(root)?;
    let db = crate::resolve_db_path(root)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let session_storage = session_id.unwrap_or(NULL_SESSION_SENTINEL);
    let conn = Connection::open(&db).with_context(|| format!("open {}", db.display()))?;
    conn.execute(
        "INSERT INTO session_shells (command, cwd, session_id, last_run_at, run_count)
         VALUES (?1, ?2, ?3, ?4, 1)
         ON CONFLICT(command, cwd, session_id) DO UPDATE SET
             last_run_at = excluded.last_run_at,
             run_count   = session_shells.run_count + 1",
        params![command, cwd, session_storage, now],
    )?;
    // Read back the post-UPSERT row so callers (notably the loop
    // detector) can decide on a single query.
    lookup_shell(&db, command, cwd, session_id).map(|opt| {
        opt.unwrap_or_else(|| ShellRecord {
            command: command.to_string(),
            cwd: cwd.to_string(),
            session_id: session_id.map(|s| s.to_string()),
            last_run_at: now,
            run_count: 1,
        })
    })
}

fn lookup_shell(
    db: &Path,
    command: &str,
    cwd: &str,
    session_id: Option<&str>,
) -> Result<Option<ShellRecord>> {
    let conn = Connection::open(db).with_context(|| format!("open {}", db.display()))?;
    let session_storage = session_id.unwrap_or(NULL_SESSION_SENTINEL);
    let row = conn
        .query_row(
            "SELECT command, cwd, session_id, last_run_at, run_count
             FROM session_shells
             WHERE command = ?1 AND cwd = ?2 AND session_id = ?3",
            params![command, cwd, session_storage],
            row_to_record,
        )
        .optional()?;
    Ok(row)
}

fn row_to_record(r: &rusqlite::Row<'_>) -> rusqlite::Result<ShellRecord> {
    let session_id = r
        .get::<_, Option<String>>(2)?
        .filter(|s| s != NULL_SESSION_SENTINEL);
    Ok(ShellRecord {
        command: r.get(0)?,
        cwd: r.get(1)?,
        session_id,
        last_run_at: r.get(3)?,
        run_count: r.get(4)?,
    })
}

/// JSON shape returned by `crabcc shell record` and the future MCP
/// tool. `record.run_count == 1` ⇔ first time we've seen this exact
/// `(command, cwd, session_id)` invocation.
pub fn record_to_json(rec: &ShellRecord) -> Value {
    json!({
        "command": rec.command,
        "cwd": rec.cwd,
        "session_id": rec.session_id,
        "last_run_at": rec.last_run_at,
        "run_count": rec.run_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ensure_test_crabcc_home;

    #[test]
    fn record_shell_inserts_with_run_count_one() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        let rec = record_shell(dir.path(), "ls -la", "/tmp", Some("s1")).unwrap();
        assert_eq!(rec.run_count, 1);
        assert_eq!(rec.command, "ls -la");
        assert_eq!(rec.cwd, "/tmp");
        assert_eq!(rec.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn record_shell_repeats_bump_run_count() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        let _ = record_shell(dir.path(), "cargo build", "/x", Some("s2")).unwrap();
        let _ = record_shell(dir.path(), "cargo build", "/x", Some("s2")).unwrap();
        let r3 = record_shell(dir.path(), "cargo build", "/x", Some("s2")).unwrap();
        assert_eq!(r3.run_count, 3, "third call must report run_count=3");
    }

    #[test]
    fn record_shell_distinct_keys_dont_collide() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        let a = record_shell(dir.path(), "ls", "/a", Some("s")).unwrap();
        let b = record_shell(dir.path(), "ls", "/b", Some("s")).unwrap();
        let c = record_shell(dir.path(), "ls", "/a", Some("other")).unwrap();
        // All three are first occurrences — different cwd or session.
        assert_eq!(a.run_count, 1);
        assert_eq!(b.run_count, 1);
        assert_eq!(c.run_count, 1);
    }

    #[test]
    fn record_shell_handles_null_session_id() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        let r1 = record_shell(dir.path(), "echo hi", "/q", None).unwrap();
        let r2 = record_shell(dir.path(), "echo hi", "/q", None).unwrap();
        assert_eq!(r1.run_count, 1);
        assert_eq!(r2.run_count, 2, "NULL-session repeats must also bump count");
        assert!(r2.session_id.is_none());
    }
}
