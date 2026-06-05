//! Typed event log on top of `_crab_event`. Five severity levels;
//! `Crash` is the highest-severity bucket and is the only one that
//! triggers crash-report packaging downstream.
//!
//! Events are append-only. Pruning is the cleanup module's job.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

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

    /// Stable integer discriminant — used by the `_crab_event.severity_int`
    /// column (#488). Order MUST stay monotonic (debug<info<warn<error<crash)
    /// so the cleanup module's `severity_int < ?` filters keep working.
    /// Adding a new variant ALWAYS goes at the end with the next value.
    pub fn as_i64(self) -> i64 {
        match self {
            Self::Debug => 0,
            Self::Info => 1,
            Self::Warn => 2,
            Self::Error => 3,
            Self::Crash => 4,
        }
    }

    pub fn from_i64(v: i64) -> Option<Self> {
        Some(match v {
            0 => Self::Debug,
            1 => Self::Info,
            2 => Self::Warn,
            3 => Self::Error,
            4 => Self::Crash,
            _ => return None,
        })
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
    /// Crate / surface name — `viz` / `agent` / `cli` /
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
    // Owned-String pre-serialize — kept for the public `record_event`
    // path that takes a `&serde_json::Value`. The watcher's heartbeat
    // tick uses `insert_with_payload_str` instead so it can reuse a
    // single buffer across ticks (#488).
    let payload_str = payload.map(serde_json::to_string).transpose()?;
    insert_with_payload_str(
        conn,
        session_id,
        severity,
        source,
        category,
        message,
        payload_str.as_deref(),
    )
}

/// Lower-level path: takes the payload as a pre-serialized JSON
/// string (or `None`). Lets per-tick callers reuse a buffer to keep
/// the heartbeat path allocation-free (#488).
pub fn insert_with_payload_str(
    conn: &Connection,
    session_id: Option<&str>,
    severity: Severity,
    source: &str,
    category: &str,
    message: &str,
    payload_str: Option<&str>,
) -> Result<i64> {
    // Prepared-statement cache (#488) — re-prepare on every call
    // would be a ~3-5× regression on the watcher's per-tick path.
    // The cache is per-Connection so the WatchHandle's dedicated
    // Godfather handle pays the prepare cost exactly once.
    //
    // Writes set BOTH columns. The legacy TEXT `severity` column has
    // a NOT NULL constraint we can't drop without a full SQLite
    // table-rebuild dance, so we write an empty string sentinel —
    // SQLite stores '' as a single type-tag byte, so the combined
    // (severity, severity_int) cost is ~2 bytes per row vs ~5-7 for
    // the old "debug"/"info"/… literals (#488). Reads prefer
    // `severity_int` and never look at `severity` for new rows.
    let mut stmt = conn.prepare_cached(
        "INSERT INTO _crab_event(ts, session_id, severity, severity_int, source, category, message, payload)
         VALUES (?1, ?2, '', ?3, ?4, ?5, ?6, ?7)",
    )?;
    let ts = now_secs();
    stmt.execute(params![
        ts as i64,
        session_id,
        severity.as_i64(),
        source,
        category,
        message,
        payload_str,
    ])?;
    Ok(conn.last_insert_rowid())
}

/// Recent events, newest first. Optional severity floor (`Some(Severity::Warn)`
/// returns warn / error / crash; `None` returns everything).
///
/// Filters branch on the new `severity_int` column when it's populated
/// (every post-migration row), falling back to the legacy `severity`
/// TEXT column for pre-migration rows. The COALESCE keeps the read
/// path branch-free per row.
pub fn list_recent(
    conn: &Connection,
    limit: usize,
    min_severity: Option<Severity>,
) -> Result<Vec<Event>> {
    // Pre-baked full SQL strings — no format!() allocation per call.
    // `prepare_cached` uses the SQL pointer as the cache key, so passing
    // a static str lets it skip even the string-compare step on warm
    // hits (#488). The OR arms cover pre-migration rows where
    // `severity_int` is NULL; schema::apply backfills on every open so
    // the OR arm is only hit on interrupted-upgrade DBs.
    const SQL_ALL: &str =
        "SELECT id, ts, session_id, severity_int, severity, source, category, message, payload
         FROM _crab_event
         ORDER BY ts DESC, id DESC
         LIMIT ?1";
    const SQL_INFO: &str =
        "SELECT id, ts, session_id, severity_int, severity, source, category, message, payload
         FROM _crab_event
         WHERE severity_int >= 1
            OR (severity_int IS NULL AND severity != '' AND severity != 'debug')
         ORDER BY ts DESC, id DESC
         LIMIT ?1";
    const SQL_WARN: &str =
        "SELECT id, ts, session_id, severity_int, severity, source, category, message, payload
         FROM _crab_event
         WHERE severity_int >= 2
            OR (severity_int IS NULL AND severity IN ('warn','error','crash'))
         ORDER BY ts DESC, id DESC
         LIMIT ?1";
    const SQL_ERROR: &str =
        "SELECT id, ts, session_id, severity_int, severity, source, category, message, payload
         FROM _crab_event
         WHERE severity_int >= 3
            OR (severity_int IS NULL AND severity IN ('error','crash'))
         ORDER BY ts DESC, id DESC
         LIMIT ?1";
    const SQL_CRASH: &str =
        "SELECT id, ts, session_id, severity_int, severity, source, category, message, payload
         FROM _crab_event
         WHERE severity_int = 4
            OR (severity_int IS NULL AND severity = 'crash')
         ORDER BY ts DESC, id DESC
         LIMIT ?1";
    let sql: &str = match min_severity {
        None | Some(Severity::Debug) => SQL_ALL,
        Some(Severity::Info) => SQL_INFO,
        Some(Severity::Warn) => SQL_WARN,
        Some(Severity::Error) => SQL_ERROR,
        Some(Severity::Crash) => SQL_CRASH,
    };
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        // INT path first: 1-byte read, no allocation. Fall through
        // to the legacy TEXT column for pre-migration rows.
        let severity = match row.get::<_, Option<i64>>(3)? {
            Some(v) => Severity::from_i64(v).unwrap_or(Severity::Info),
            None => row
                .get_ref(4)?
                .as_str()
                .ok()
                .and_then(Severity::parse_str)
                .unwrap_or(Severity::Info),
        };
        let payload_str: Option<String> = row.get(8)?;
        Ok(Event {
            id: row.get(0)?,
            ts: row.get::<_, i64>(1)? as u64,
            session_id: row.get(2)?,
            severity,
            source: row.get(5)?,
            category: row.get(6)?,
            message: row.get(7)?,
            payload: payload_str.and_then(|s| serde_json::from_str(&s).ok()),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn now_secs() -> u64 {
    crabcc_core::time::unix_now_secs()
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
