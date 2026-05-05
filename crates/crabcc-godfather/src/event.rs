//! Typed event log on top of `_crab_event`. Five severity levels;
//! `Crash` is the highest-severity bucket and is the only one that
//! triggers crash-report packaging downstream.
//!
//! Events are append-only. Pruning is the cleanup module's job.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Debug,
    Info,
    Warn,
    Error,
    Crash,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Crash => "crash",
        }
    }

    /// Parse from the lowercase string form. Named `parse_str` (not
    /// `from_str`) to avoid clashing with `std::str::FromStr`'s method
    /// signature — we don't want to commit to its `Err` associated type
    /// (which downstream callers might want to match on as a typed
    /// error) just to be parseable from `str`.
    pub fn parse_str(s: &str) -> Option<Self> {
        Some(match s {
            "debug" => Self::Debug,
            "info" => Self::Info,
            "warn" => Self::Warn,
            "error" => Self::Error,
            "crash" => Self::Crash,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub ts: u64,
    pub session_id: Option<String>,
    pub severity: Severity,
    /// Crate / surface name — `viz` / `desktop` / `agent` / `cli` /
    /// `godfather` itself for heartbeats and supervisor lifecycle.
    pub source: String,
    /// Sub-classifier used for filtering — `heartbeat` / `crash` /
    /// `lifecycle` / `network` / `db` etc.
    pub category: String,
    pub message: String,
    pub payload: Option<serde_json::Value>,
}

pub fn insert(
    conn: &Connection,
    session_id: Option<&str>,
    severity: Severity,
    source: &str,
    category: &str,
    message: &str,
    payload: Option<&serde_json::Value>,
) -> Result<i64> {
    // Prepared-statement cache (#488) — re-prepare on every call
    // would be a ~3-5× regression on the watcher's per-tick path.
    // The cache is per-Connection so the WatchHandle's dedicated
    // Godfather handle pays the prepare cost exactly once.
    let mut stmt = conn.prepare_cached(
        "INSERT INTO _crab_event(ts, session_id, severity, source, category, message, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    let ts = now_secs();
    let payload_str = payload.map(serde_json::to_string).transpose()?;
    stmt.execute(params![
        ts as i64,
        session_id,
        severity.as_str(),
        source,
        category,
        message,
        payload_str,
    ])?;
    Ok(conn.last_insert_rowid())
}

/// Recent events, newest first. Optional severity floor (`Some(Severity::Warn)`
/// returns warn / error / crash; `None` returns everything).
pub fn list_recent(
    conn: &Connection,
    limit: usize,
    min_severity: Option<Severity>,
) -> Result<Vec<Event>> {
    let sev_clause = min_severity
        .map(|s| match s {
            Severity::Debug => "",
            Severity::Info => " WHERE severity != 'debug'",
            Severity::Warn => " WHERE severity IN ('warn','error','crash')",
            Severity::Error => " WHERE severity IN ('error','crash')",
            Severity::Crash => " WHERE severity = 'crash'",
        })
        .unwrap_or("");
    let sql = format!(
        "SELECT id, ts, session_id, severity, source, category, message, payload
         FROM _crab_event{sev_clause}
         ORDER BY ts DESC, id DESC
         LIMIT ?1"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        // Borrow the severity column as &str instead of allocating a
        // String per row — `Severity::parse_str` only needs a slice
        // (#488: skip-String-clone). Falls back to Info if the row
        // has a value the enum doesn't know (forward-compat for a
        // future severity bump).
        let severity = row
            .get_ref(3)?
            .as_str()
            .ok()
            .and_then(Severity::parse_str)
            .unwrap_or(Severity::Info);
        let payload_str: Option<String> = row.get(7)?;
        Ok(Event {
            id: row.get(0)?,
            ts: row.get::<_, i64>(1)? as u64,
            session_id: row.get(2)?,
            severity,
            source: row.get(4)?,
            category: row.get(5)?,
            message: row.get(6)?,
            payload: payload_str.and_then(|s| serde_json::from_str(&s).ok()),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
        let conn = Connection::open_in_memory().unwrap();
        schema::apply(&conn).unwrap();
        conn
    }

    #[test]
    fn insert_then_list_recent_roundtrip() {
        let conn = open_inmem();
        let id = insert(
            &conn,
            None,
            Severity::Warn,
            "viz",
            "lifecycle",
            "shutdown requested",
            None,
        )
        .unwrap();
        assert!(id > 0);
        let events = list_recent(&conn, 10, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].severity, Severity::Warn);
        assert_eq!(events[0].source, "viz");
    }

    #[test]
    fn min_severity_filters_correctly() {
        let conn = open_inmem();
        for (sev, msg) in [
            (Severity::Debug, "d"),
            (Severity::Info, "i"),
            (Severity::Warn, "w"),
            (Severity::Error, "e"),
            (Severity::Crash, "c"),
        ] {
            insert(&conn, None, sev, "test", "lifecycle", msg, None).unwrap();
        }
        assert_eq!(list_recent(&conn, 100, None).unwrap().len(), 5);
        assert_eq!(
            list_recent(&conn, 100, Some(Severity::Info)).unwrap().len(),
            4
        );
        assert_eq!(
            list_recent(&conn, 100, Some(Severity::Warn)).unwrap().len(),
            3
        );
        assert_eq!(
            list_recent(&conn, 100, Some(Severity::Error))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            list_recent(&conn, 100, Some(Severity::Crash))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn payload_roundtrip_through_json() {
        let conn = open_inmem();
        let payload = serde_json::json!({"pid": 1234, "rss_mb": 256});
        insert(
            &conn,
            None,
            Severity::Info,
            "godfather",
            "heartbeat",
            "watch tick",
            Some(&payload),
        )
        .unwrap();
        let evts = list_recent(&conn, 1, None).unwrap();
        assert_eq!(evts[0].payload, Some(payload));
    }

    #[test]
    fn severity_strings_round_trip() {
        for s in [
            Severity::Debug,
            Severity::Info,
            Severity::Warn,
            Severity::Error,
            Severity::Crash,
        ] {
            assert_eq!(Severity::parse_str(s.as_str()), Some(s));
        }
        assert_eq!(Severity::parse_str("nope"), None);
    }
}
