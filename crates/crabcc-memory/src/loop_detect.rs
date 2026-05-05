//! Loop detector — flags `(path, session_id)` and `(command, cwd,
//! session_id)` pairs whose `read_count` / `run_count` cross a
//! threshold. Reads from `session_reads` and `session_shells`, both
//! populated by [`crate::read::compute`] and
//! [`crate::shell::record_shell`].
//!
//! Surface:
//!
//! - [`detect`] — query, return a `Vec<LoopHit>` for ad-hoc
//!   inspection. CLI exposes this as `crabcc loop check`.
//! - [`DEFAULT_THRESHOLD`] — 5. Tuned for "agent stuck re-reading
//!   the same file" / "agent re-running cargo build over and over".
//!   Lower than 5 noise-traps obvious patterns; higher misses.
//!
//! No automatic warning emission lives here — that's a boundary
//! concern. CLI / hook code reads the [`ShellRecord::run_count`] /
//! payload's `read_count` and emits stderr if needed.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;

/// Default loop threshold — repeats at or above this count surface
/// in [`detect`]'s result. The Bash hook + read CLI use the same
/// constant so warnings line up with `crabcc loop check`.
pub const DEFAULT_THRESHOLD: i64 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LoopKind {
    Read,
    Shell,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoopHit {
    pub kind: LoopKind,
    /// File path for `Read`, command (truncated) for `Shell`.
    pub key: String,
    /// Optional context — `cwd` for shells, empty for reads.
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    pub count: i64,
    pub last_seen_at: i64,
}

/// Find loops above `threshold`. When `session_id` is Some, only
/// rows matching that session are returned; None returns rows from
/// every session in the ledger. Empty result == no loops.
pub fn detect(root: &Path, session_id: Option<&str>, threshold: i64) -> Result<Vec<LoopHit>> {
    // Touch the db so the schema migration runs if this is the
    // first call. `Palace::open` is idempotent.
    let _ = crate::Palace::open(root)?;
    let db = crate::resolve_db_path(root)?;
    let conn = Connection::open(&db).with_context(|| format!("open {}", db.display()))?;

    let mut hits = Vec::new();
    hits.extend(query_reads(&conn, session_id, threshold)?);
    hits.extend(query_shells(&conn, session_id, threshold)?);
    // Sort newest-first so the most actionable hits surface first.
    hits.sort_by_key(|h| std::cmp::Reverse(h.last_seen_at));
    Ok(hits)
}

/// JSON shape — `Vec<LoopHit>` serialized via serde.
pub fn hits_to_json(hits: &[LoopHit]) -> Value {
    json!(hits)
}

fn query_reads(
    conn: &Connection,
    session_id: Option<&str>,
    threshold: i64,
) -> Result<Vec<LoopHit>> {
    let mut out = Vec::new();
    let sql = match session_id {
        Some(_) => {
            "SELECT path, session_id, read_count, served_at FROM session_reads
             WHERE read_count >= ?1 AND session_id = ?2"
        }
        None => {
            "SELECT path, session_id, read_count, served_at FROM session_reads
             WHERE read_count >= ?1"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let mapper = |r: &rusqlite::Row<'_>| -> rusqlite::Result<LoopHit> {
        Ok(LoopHit {
            kind: LoopKind::Read,
            key: r.get(0)?,
            cwd: None,
            session_id: r.get(1)?,
            count: r.get(2)?,
            last_seen_at: r.get(3)?,
        })
    };
    match session_id {
        Some(sid) => {
            let rows = stmt.query_map(params![threshold, sid], mapper)?;
            for hit in rows {
                out.push(hit?);
            }
        }
        None => {
            let rows = stmt.query_map(params![threshold], mapper)?;
            for hit in rows {
                out.push(hit?);
            }
        }
    }
    Ok(out)
}

fn query_shells(
    conn: &Connection,
    session_id: Option<&str>,
    threshold: i64,
) -> Result<Vec<LoopHit>> {
    let mut out = Vec::new();
    let sql = match session_id {
        Some(_) => {
            "SELECT command, cwd, session_id, run_count, last_run_at FROM session_shells
             WHERE run_count >= ?1 AND session_id = ?2"
        }
        None => {
            "SELECT command, cwd, session_id, run_count, last_run_at FROM session_shells
             WHERE run_count >= ?1"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    match session_id {
        Some(sid) => {
            let rows = stmt.query_map(params![threshold, sid], shell_hit_from_row)?;
            for hit in rows {
                out.push(hit?);
            }
        }
        None => {
            let rows = stmt.query_map(params![threshold], shell_hit_from_row)?;
            for hit in rows {
                out.push(hit?);
            }
        }
    }
    Ok(out)
}

fn shell_hit_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<LoopHit> {
    let raw_session: Option<String> = r.get(2)?;
    // shell.rs uses `""` as the NULL-session sentinel — surface as
    // `None` to callers so they don't see a phantom empty string.
    let session_id = raw_session.filter(|s| !s.is_empty());
    Ok(LoopHit {
        kind: LoopKind::Shell,
        key: r.get(0)?,
        cwd: Some(r.get(1)?),
        session_id,
        count: r.get(3)?,
        last_seen_at: r.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ensure_test_crabcc_home;

    #[test]
    fn detect_below_threshold_returns_empty() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        // One shell row at run_count=1.
        let _ = crate::shell::record_shell(dir.path(), "ls", "/tmp", Some("s")).unwrap();
        let hits = detect(dir.path(), Some("s"), DEFAULT_THRESHOLD).unwrap();
        assert!(hits.is_empty(), "no hits below threshold");
    }

    #[test]
    fn detect_at_threshold_returns_hit() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..DEFAULT_THRESHOLD {
            let _ =
                crate::shell::record_shell(dir.path(), "cargo build", "/x", Some("loop")).unwrap();
        }
        let hits = detect(dir.path(), Some("loop"), DEFAULT_THRESHOLD).unwrap();
        assert_eq!(hits.len(), 1, "exactly one hit at threshold");
        let hit = &hits[0];
        assert_eq!(hit.kind, LoopKind::Shell);
        assert_eq!(hit.key, "cargo build");
        assert_eq!(hit.cwd.as_deref(), Some("/x"));
        assert_eq!(hit.session_id.as_deref(), Some("loop"));
        assert_eq!(hit.count, DEFAULT_THRESHOLD);
    }

    #[test]
    fn detect_session_filter_isolates_to_caller() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..DEFAULT_THRESHOLD {
            let _ =
                crate::shell::record_shell(dir.path(), "cmd-a", "/x", Some("session-a")).unwrap();
        }
        for _ in 0..DEFAULT_THRESHOLD {
            let _ =
                crate::shell::record_shell(dir.path(), "cmd-b", "/x", Some("session-b")).unwrap();
        }
        let hits_a = detect(dir.path(), Some("session-a"), DEFAULT_THRESHOLD).unwrap();
        assert_eq!(hits_a.len(), 1);
        assert_eq!(hits_a[0].key, "cmd-a");

        let hits_all = detect(dir.path(), None, DEFAULT_THRESHOLD).unwrap();
        assert_eq!(hits_all.len(), 2, "no session filter sees both");
    }
}
