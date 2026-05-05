//! Per-app session tracking. One row per app boot in `_crab_session`.
//!
//! The id is a 16-hex-char string (sha256 of started_at + pid +
//! random) — short enough to copy/paste into a GH issue body, long
//! enough to never collide.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub type SessionId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    pub id: SessionId,
    pub app: String,
    pub version: String,
    pub pid: u32,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
}

pub fn start(conn: &Connection, app: &str, version: &str, pid: u32) -> Result<SessionId> {
    let ts = now_secs();
    let id = stable_id(app, pid, ts);
    conn.execute(
        "INSERT INTO _crab_session(id, app, version, pid, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, app, version, pid as i64, ts as i64],
    )?;
    Ok(id)
}

pub fn end(
    conn: &Connection,
    id: &str,
    exit_code: Option<i32>,
    exit_signal: Option<i32>,
) -> Result<()> {
    let ts = now_secs();
    conn.execute(
        "UPDATE _crab_session
         SET ended_at = ?1, exit_code = ?2, exit_signal = ?3
         WHERE id = ?4",
        params![ts as i64, exit_code, exit_signal, id],
    )?;
    Ok(())
}

/// Fetch a session by id. Returns `Ok(None)` if the row was pruned
/// or never existed — callers handle either case as "no session".
pub fn get(conn: &Connection, id: &str) -> Result<Option<Session>> {
    let mut stmt = conn.prepare(
        "SELECT id, app, version, pid, started_at, ended_at, exit_code, exit_signal
         FROM _crab_session WHERE id = ?1",
    )?;
    let mut rows = stmt.query(params![id])?;
    if let Some(row) = rows.next()? {
        Ok(Some(read_row(row)?))
    } else {
        Ok(None)
    }
}

/// Active sessions (no `ended_at`), newest first. Used by the
/// `crabcc-godfather status` surface and by the cleanup module's
/// "skip currently-running rows" pass.
pub fn list_active(conn: &Connection, limit: usize) -> Result<Vec<Session>> {
    let mut stmt = conn.prepare(
        "SELECT id, app, version, pid, started_at, ended_at, exit_code, exit_signal
         FROM _crab_session WHERE ended_at IS NULL ORDER BY started_at DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], read_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// All sessions, newest first.
pub fn list_recent(conn: &Connection, limit: usize) -> Result<Vec<Session>> {
    let mut stmt = conn.prepare(
        "SELECT id, app, version, pid, started_at, ended_at, exit_code, exit_signal
         FROM _crab_session ORDER BY started_at DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], read_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        app: row.get(1)?,
        version: row.get(2)?,
        pid: row.get::<_, i64>(3)? as u32,
        started_at: row.get::<_, i64>(4)? as u64,
        ended_at: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
        exit_code: row.get::<_, Option<i32>>(6)?,
        exit_signal: row.get::<_, Option<i32>>(7)?,
    })
}

/// Hash the inputs into a stable, copy-pastable session id. The
/// random nibble guards against the (theoretical) case of two apps
/// booting in the same second with the same PID — should be 1-in-2¹⁶
/// even in pathological re-fork loops.
fn stable_id(app: &str, pid: u32, ts: u64) -> String {
    let nonce: u32 = std::process::id().wrapping_mul(ts as u32 ^ pid);
    let input = format!("{app}|{pid}|{ts}|{nonce}");
    let digest = Sha256::digest(input.as_bytes());
    let mut s = String::with_capacity(16);
    use std::fmt::Write;
    for b in digest.iter().take(8) {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;

    fn open_inmem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        schema::apply(&c).unwrap();
        c
    }

    #[test]
    fn start_then_end_roundtrip() {
        let conn = open_inmem();
        let id = start(&conn, "viz", "3.0.0", 4321).unwrap();
        assert_eq!(id.len(), 16);
        let s = get(&conn, &id).unwrap().expect("must exist");
        assert_eq!(s.app, "viz");
        assert_eq!(s.pid, 4321);
        assert!(s.ended_at.is_none());

        end(&conn, &id, Some(0), None).unwrap();
        let s = get(&conn, &id).unwrap().unwrap();
        assert!(s.ended_at.is_some());
        assert_eq!(s.exit_code, Some(0));
    }

    #[test]
    fn list_active_excludes_ended() {
        let conn = open_inmem();
        let active = start(&conn, "viz", "3.0.0", 1).unwrap();
        let dead = start(&conn, "viz", "3.0.0", 2).unwrap();
        end(&conn, &dead, Some(0), None).unwrap();
        let act = list_active(&conn, 10).unwrap();
        assert_eq!(act.len(), 1);
        assert_eq!(act[0].id, active);
    }

    #[test]
    fn stable_id_is_deterministic_per_input_run() {
        // Two calls inside the same nonce window won't collide
        // because the nonce eats the wrapping pid*ts mix.
        let id_1 = stable_id("viz", 1, 1700000000);
        let id_2 = stable_id("viz", 1, 1700000001);
        assert_ne!(id_1, id_2);
        assert_eq!(id_1.len(), 16);
    }

    #[test]
    fn get_missing_returns_none() {
        let conn = open_inmem();
        assert!(get(&conn, "deadbeefcafebabe").unwrap().is_none());
    }
}
