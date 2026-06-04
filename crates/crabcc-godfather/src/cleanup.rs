//! Recurring DB-cleanup jobs. The Godfather runs `prune_if_due()`
//! lazily on `open()` (skipped if the previous prune was less than
//! `prune_interval_secs` ago) so embedded callers never block on it.
//!
//! ## Retention windows
//!
//! Sensible defaults — every consumer can override via
//! `Retention::custom`. Crash records are intentionally kept much
//! longer than samples / events because they're the inputs to GH
//! issue creation and a stale crash is still useful three months
//! later.
//!
//! | Table | Default retention | Reason |
//! |---|---|---|
//! | `_crab_event` (severity ≠ crash) | 30 days | Glance-window for "what did the agents do this week" |
//! | `_crab_resource_sample` | 7 days | Trace-shaped data; the live dashboard cares about ~minutes, not weeks |
//! | `_crab_session` (ended) | 90 days | Useful for "what version did I run last quarter" |
//! | `_crab_crash` | 365 days | Long-tail debugging; survive a release-cycle's worth |
//!
//! Active rows (sessions with `ended_at IS NULL`) are NEVER pruned;
//! we wouldn't want to drop the heartbeat trail of a long-running
//! supervisor.
//!
//! ## VACUUM
//!
//! After a prune that touched > 0 rows, we run an incremental
//! `PRAGMA wal_checkpoint(PASSIVE)` to keep the WAL bounded. A full
//! `VACUUM` only fires once a week (gated on the `last_vacuumed_at`
//! metadata key) — it briefly takes an exclusive lock and we don't
//! want every embedded boot to pay for it.

use anyhow::Result;
use rusqlite::{params, Connection};

/// Per-table retention windows in seconds. `from_secs` accepts
/// human-readable arithmetic (`60 * 60 * 24 * 30`) at the call site.
#[derive(Debug, Clone, Copy)]
pub struct Retention {
    pub event_secs: i64,
    pub resource_secs: i64,
    pub session_secs: i64,
    pub crash_secs: i64,
    /// How often `prune_if_due` actually runs; bumping this from the
    /// default 24 h cuts I/O for short-lived CLI invocations that
    /// import the lib for one operation.
    pub prune_interval_secs: i64,
    /// How often a full `VACUUM` runs (default: weekly).
    pub vacuum_interval_secs: i64,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            event_secs: 30 * 86_400,
            resource_secs: 7 * 86_400,
            session_secs: 90 * 86_400,
            crash_secs: 365 * 86_400,
            prune_interval_secs: 86_400,
            vacuum_interval_secs: 7 * 86_400,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PruneStats {
    pub events_deleted: usize,
    pub samples_deleted: usize,
    pub sessions_deleted: usize,
    pub crashes_deleted: usize,
    pub vacuumed: bool,
    pub skipped: bool,
}

pub fn prune_if_due(conn: &Connection, retention: &Retention) -> Result<PruneStats> {
    let now = now_secs();
    let last = read_metadata_i64(conn, "last_pruned_at").unwrap_or_default();
    if now - last < retention.prune_interval_secs {
        return Ok(PruneStats {
            skipped: true,
            ..Default::default()
        });
    }
    let stats = prune_now(conn, retention)?;
    write_metadata_i64(conn, "last_pruned_at", now)?;
    Ok(stats)
}

/// Force a prune regardless of the last-pruned timestamp. Used by
/// the `crabcc-godfather prune` CLI subcommand.
pub fn prune_now(conn: &Connection, retention: &Retention) -> Result<PruneStats> {
    let now = now_secs();
    let event_cutoff = now - retention.event_secs;
    let resource_cutoff = now - retention.resource_secs;
    let session_cutoff = now - retention.session_secs;
    let crash_cutoff = now - retention.crash_secs;

    // Order matters — delete leaf rows first (samples / events / crashes)
    // before sessions, so foreign-key-shaped joins by id keep working
    // for the duration of the wall-clock window.
    //
    // Wrap the four DELETEs in a single transaction (#488): each
    // `conn.execute` would otherwise be its own implicit transaction →
    // four fsyncs per prune. Coalescing into one TX cuts that to a
    // single fsync. VACUUM below stays outside the TX (it can't run
    // inside one).
    let tx = conn.unchecked_transaction()?;
    let samples_deleted = tx.execute(
        "DELETE FROM _crab_resource_sample WHERE ts < ?1",
        params![resource_cutoff],
    )?;
    // Filter on `severity_int` first (1-byte INTEGER comparison) and
    // fall back to the legacy TEXT column for any pre-migration rows
    // schema::apply hasn't backfilled yet (#488). Empty-string `severity`
    // is the new-row sentinel — it's never 'crash', so ignoring the
    // TEXT branch when severity_int is set is correct.
    let events_deleted = tx.execute(
        "DELETE FROM _crab_event
         WHERE ts < ?1
           AND (severity_int IS NOT NULL AND severity_int != 4
                OR severity_int IS NULL AND severity != 'crash')",
        params![event_cutoff],
    )?;
    let crashes_deleted = tx.execute(
        "DELETE FROM _crab_crash WHERE ts < ?1",
        params![crash_cutoff],
    )?;
    // Only drop sessions whose `ended_at` is also stale — never drop
    // an active session even if it was started long ago (a supervisor
    // running for months is a feature, not a leak).
    let sessions_deleted = tx.execute(
        "DELETE FROM _crab_session
             WHERE ended_at IS NOT NULL AND ended_at < ?1",
        params![session_cutoff],
    )?;
    tx.commit()?;

    let mut stats = PruneStats {
        events_deleted,
        samples_deleted,
        sessions_deleted,
        crashes_deleted,
        vacuumed: false,
        skipped: false,
    };

    if events_deleted + samples_deleted + sessions_deleted + crashes_deleted > 0 {
        // Cheap, doesn't take exclusive lock.
        conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);")?;
    }

    let last_vac = read_metadata_i64(conn, "last_vacuumed_at").unwrap_or_default();
    if now - last_vac >= retention.vacuum_interval_secs {
        // Full VACUUM — exclusive lock, but fast on a few-MB DB. Not
        // wrapped in a transaction (VACUUM can't be).
        conn.execute_batch("VACUUM;")?;
        write_metadata_i64(conn, "last_vacuumed_at", now)?;
        stats.vacuumed = true;
    }

    Ok(stats)
}

fn read_metadata_i64(conn: &Connection, key: &str) -> Option<i64> {
    conn.query_row(
        "SELECT value FROM _crab_metadata WHERE key = ?1",
        params![key],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .and_then(|s| s.parse().ok())
}

fn write_metadata_i64(conn: &Connection, key: &str, value: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO _crab_metadata(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value.to_string()],
    )?;
    Ok(())
}

fn now_secs() -> i64 {
    crabcc_core::time::unix_now_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{event, schema, session};

    fn open_inmem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        schema::apply(&c).unwrap();
        c
    }

    /// Helper: pre-date a row by fiddling its `ts`. Tests need to
    /// drive the clock forward without sleeping for actual days.
    fn set_event_ts(conn: &Connection, id: i64, ts: i64) {
        conn.execute(
            "UPDATE _crab_event SET ts = ?1 WHERE id = ?2",
            params![ts, id],
        )
        .unwrap();
    }

    #[test]
    fn prune_keeps_recent_drops_old_events() {
        let conn = open_inmem();
        let now = now_secs();

        // Insert a recent event and a "31-days-ago" event.
        let recent = event::insert(
            &conn,
            None,
            event::Severity::Info,
            "viz",
            "x",
            "recent",
            None,
        )
        .unwrap();
        let old =
            event::insert(&conn, None, event::Severity::Info, "viz", "x", "old", None).unwrap();
        set_event_ts(&conn, old, now - 31 * 86_400);

        let stats = prune_now(&conn, &Retention::default()).unwrap();
        assert_eq!(stats.events_deleted, 1);

        // Recent survives; old is gone.
        let surviving = event::list_recent(&conn, 10, None).unwrap();
        assert_eq!(surviving.len(), 1);
        assert_eq!(surviving[0].id, recent);
    }

    #[test]
    fn prune_keeps_crash_events_beyond_event_window() {
        let conn = open_inmem();
        let now = now_secs();

        // 60 days ago — past event_secs (30) but inside crash retention (365).
        let id = event::insert(
            &conn,
            None,
            event::Severity::Crash,
            "viz",
            "panic",
            "boom",
            None,
        )
        .unwrap();
        set_event_ts(&conn, id, now - 60 * 86_400);

        let stats = prune_now(&conn, &Retention::default()).unwrap();
        assert_eq!(stats.events_deleted, 0);
        assert_eq!(event::list_recent(&conn, 10, None).unwrap().len(), 1);
    }

    #[test]
    fn prune_skips_active_sessions() {
        let conn = open_inmem();
        let now = now_secs();

        // Two-year-old active session — must be kept (no ended_at).
        let active = session::start(&conn, "godfather", "3.0", 1).unwrap();
        conn.execute(
            "UPDATE _crab_session SET started_at = ?1 WHERE id = ?2",
            params![now - 730 * 86_400, active],
        )
        .unwrap();

        // Old, ended session — must be pruned (90-day window).
        let dead = session::start(&conn, "viz", "3.0", 2).unwrap();
        session::end(&conn, &dead, Some(0), None).unwrap();
        conn.execute(
            "UPDATE _crab_session SET ended_at = ?1 WHERE id = ?2",
            params![now - 100 * 86_400, dead],
        )
        .unwrap();

        let stats = prune_now(&conn, &Retention::default()).unwrap();
        assert_eq!(stats.sessions_deleted, 1);
        assert!(session::get(&conn, &active).unwrap().is_some());
        assert!(session::get(&conn, &dead).unwrap().is_none());
    }

    #[test]
    fn prune_if_due_skips_when_recently_pruned() {
        let conn = open_inmem();
        // First call — runs.
        let s = prune_if_due(&conn, &Retention::default()).unwrap();
        assert!(!s.skipped);
        // Second call — within prune_interval, must skip.
        let s = prune_if_due(&conn, &Retention::default()).unwrap();
        assert!(s.skipped);
    }

    #[test]
    fn vacuum_runs_on_first_call() {
        let conn = open_inmem();
        let stats = prune_now(&conn, &Retention::default()).unwrap();
        assert!(stats.vacuumed);
    }
}
