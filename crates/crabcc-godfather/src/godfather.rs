//! `Godfather` — the main facade. Embedded callers (cli / desktop /
//! viz) construct one at startup, record their session, drop it on
//! exit. Everything's a thin wrapper around the per-table modules.
//!
//! ## Privacy opt-out
//!
//! `CRABCC_NO_TELEMETRY=1` short-circuits every write at construction:
//! `Godfather::open` returns a fully-no-op handle. Reads still work
//! (so a tool like `crabcc-godfather status` can render an empty
//! state) but no `_crab_event`, `_crab_session`, or
//! `_crab_resource_sample` row will ever land.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::cleanup::{self, Retention};
use crate::event::{self, Event, Severity};
use crate::host::HostInfo;
use crate::session::{self, Session, SessionId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallSource {
    /// `cargo install crabcc` from crates.io or a path.
    Cargo,
    /// GitHub release artifact.
    GithubRelease,
    /// Homebrew tap.
    Homebrew,
    /// Built locally from the repo (`cargo build`, dev workflow).
    Source,
    /// Anything else / unknown.
    Other,
}

impl InstallSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::GithubRelease => "github_release",
            Self::Homebrew => "homebrew",
            Self::Source => "source",
            Self::Other => "other",
        }
    }
}

/// One element of a batched `record_resource_samples` call. Cheap to
/// construct in a tight loop; the watcher buffers a small Vec of
/// these between flushes.
#[derive(Debug, Clone, Copy)]
pub struct ResourceSample {
    pub ts: u64,
    pub rss_mb: u64,
    pub cpu_pct: f32,
    pub vsize_mb: u64,
}

/// Shared facade across the lib + the CLI binary.
pub struct Godfather {
    conn: Connection,
    /// Mirrored from `CRABCC_NO_TELEMETRY` at construction so every
    /// `record_*` is a single field check, not an env-var read.
    telemetry_enabled: bool,
}

impl Godfather {
    /// Open `~/.crabcc/_internal.db` (or `$CRABCC_HOME/_internal.db`
    /// when set), apply migrations, run the lazy prune. Idempotent.
    pub fn open() -> Result<Self> {
        Self::open_at(&default_db_path()?)
    }

    /// Same, but at an explicit path — useful for tests + CLI
    /// `--db /tmp/foo.db` overrides.
    pub fn open_at(path: &Path) -> Result<Self> {
        Self::open_at_with(path, std::env::var_os("CRABCC_NO_TELEMETRY").is_none())
    }

    /// Lower-level constructor that takes the telemetry flag
    /// explicitly. Lets tests opt-out without mutating the global
    /// env (process-wide `set_var` races with parallel tests).
    pub fn open_at_with(path: &Path, telemetry_enabled: bool) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open godfather db {}", path.display()))?;
        crate::schema::apply(&conn)?;
        // Lazy prune. If we somehow fail (disk full / locked), don't
        // wedge the caller — log and continue with a working handle.
        if telemetry_enabled {
            if let Err(e) = cleanup::prune_if_due(&conn, &Retention::default()) {
                tracing::warn!(target: "crabcc_godfather", error = %e, "lazy prune skipped");
            }
        }
        Ok(Self {
            conn,
            telemetry_enabled,
        })
    }

    /// Idempotent: written exactly once, on first open of a brand-new
    /// install. Subsequent calls compare-and-swap so the original
    /// install_time stays the source of truth.
    pub fn record_install_once(&self, version: &str, source: InstallSource) -> Result<()> {
        if !self.telemetry_enabled {
            return Ok(());
        }
        let ts = now_secs();
        // INSERT-OR-IGNORE so the first writer wins.
        self.conn.execute(
            "INSERT INTO _crab_metadata(key, value) VALUES ('install_time', ?1)
             ON CONFLICT(key) DO NOTHING",
            rusqlite::params![ts.to_string()],
        )?;
        self.conn.execute(
            "INSERT INTO _crab_metadata(key, value) VALUES ('install_version', ?1)
             ON CONFLICT(key) DO NOTHING",
            rusqlite::params![version],
        )?;
        self.conn.execute(
            "INSERT INTO _crab_metadata(key, value) VALUES ('install_source', ?1)
             ON CONFLICT(key) DO NOTHING",
            rusqlite::params![source.as_str()],
        )?;
        Ok(())
    }

    /// Refresh the `_crab_host` row with current OS / capacity state.
    /// Cheap; called on every open so OS upgrades surface in the
    /// next crash report without manual intervention.
    pub fn record_host_info(&self) -> Result<HostInfo> {
        let info = HostInfo::collect();
        if !self.telemetry_enabled {
            return Ok(info);
        }
        self.conn.execute(
            "INSERT INTO _crab_host(id, os, os_version, arch, cpu_count, total_memory_mb, \
                                    hostname_hash, machine_id_hash, updated_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                os = excluded.os,
                os_version = excluded.os_version,
                arch = excluded.arch,
                cpu_count = excluded.cpu_count,
                total_memory_mb = excluded.total_memory_mb,
                hostname_hash = excluded.hostname_hash,
                machine_id_hash = excluded.machine_id_hash,
                updated_at = excluded.updated_at",
            rusqlite::params![
                info.os,
                info.os_version,
                info.arch,
                info.cpu_count,
                info.total_memory_mb,
                info.hostname_hash,
                info.machine_id_hash,
                now_secs() as i64,
            ],
        )?;
        Ok(info)
    }

    pub fn record_session_start(&self, app: &str, version: &str, pid: u32) -> Result<SessionId> {
        if !self.telemetry_enabled {
            return Ok(String::from("disabled"));
        }
        session::start(&self.conn, app, version, pid)
    }

    pub fn record_session_end(
        &self,
        id: &str,
        exit_code: Option<i32>,
        exit_signal: Option<i32>,
    ) -> Result<()> {
        if !self.telemetry_enabled {
            return Ok(());
        }
        session::end(&self.conn, id, exit_code, exit_signal)
    }

    pub fn record_event(
        &self,
        session_id: Option<&str>,
        severity: Severity,
        source: &str,
        category: &str,
        message: &str,
        payload: Option<&serde_json::Value>,
    ) -> Result<i64> {
        if !self.telemetry_enabled {
            return Ok(0);
        }
        event::insert(
            &self.conn, session_id, severity, source, category, message, payload,
        )
    }

    /// Same shape as [`record_event`] but takes the payload as a
    /// pre-serialized JSON string. Lets per-tick callers (the
    /// supervisor's heartbeat) reuse one buffer across ticks instead
    /// of allocating a fresh `serde_json::Map` + `String` per call.
    /// See `WatchHandle::run` for the buffer-reuse pattern (#488).
    pub fn record_event_with_payload_str(
        &self,
        session_id: Option<&str>,
        severity: Severity,
        source: &str,
        category: &str,
        message: &str,
        payload_str: Option<&str>,
    ) -> Result<i64> {
        if !self.telemetry_enabled {
            return Ok(0);
        }
        event::insert_with_payload_str(
            &self.conn,
            session_id,
            severity,
            source,
            category,
            message,
            payload_str,
        )
    }

    pub fn record_resource_sample(
        &self,
        session_id: &str,
        rss_mb: u64,
        cpu_pct: f32,
        vsize_mb: u64,
    ) -> Result<()> {
        if !self.telemetry_enabled {
            return Ok(());
        }
        // The watcher's per-tick path (#488). `prepare_cached` keeps
        // the parsed statement on the Connection's cache so subsequent
        // ticks pay zero re-parse cost — measured ~5× speedup.
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO _crab_resource_sample(session_id, ts, rss_mb, cpu_pct, vsize_mb)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        let ts = now_secs() as i64;
        stmt.execute(rusqlite::params![
            session_id,
            ts,
            rss_mb as i64,
            cpu_pct as f64,
            vsize_mb as i64
        ])?;
        Ok(())
    }

    /// Bulk-insert N samples in a single transaction. Issue #488:
    /// per-call SQLite COMMIT (fsync) is the dominant cost on the
    /// watcher's per-tick path; coalescing 6 samples into one
    /// transaction cuts the amortised cost by ~5×. Caller is expected
    /// to buffer samples in memory between flushes.
    ///
    /// Slice form rather than `IntoIterator` so the caller decides
    /// the buffer shape — `WatchHandle` uses a small `Vec` it
    /// re-clears per flush, keeping allocations stable.
    pub fn record_resource_samples(
        &self,
        session_id: &str,
        samples: &[ResourceSample],
    ) -> Result<()> {
        if !self.telemetry_enabled || samples.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO _crab_resource_sample(session_id, ts, rss_mb, cpu_pct, vsize_mb)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for s in samples {
                stmt.execute(rusqlite::params![
                    session_id,
                    s.ts as i64,
                    s.rss_mb as i64,
                    s.cpu_pct as f64,
                    s.vsize_mb as i64
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Append a `_crab_crash` row tied to a session. Caller is
    /// expected to have already recorded a `Severity::Crash` event;
    /// this row carries the structured exit data the crash-report
    /// builder uses.
    pub fn record_crash(
        &self,
        session_id: &str,
        exit_code: Option<i32>,
        exit_signal: Option<i32>,
        log_tail: Option<&str>,
    ) -> Result<i64> {
        if !self.telemetry_enabled {
            return Ok(0);
        }
        let ts = now_secs() as i64;
        self.conn.execute(
            "INSERT INTO _crab_crash(session_id, ts, exit_code, exit_signal, log_tail)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![session_id, ts, exit_code, exit_signal, log_tail],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    // ── read-side surface ────────────────────────────────────────

    pub fn host_info(&self) -> Result<Option<HostInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT os, os_version, arch, cpu_count, total_memory_mb, hostname_hash, machine_id_hash
             FROM _crab_host WHERE id = 1",
        )?;
        let mut rows = stmt.query([])?;
        if let Some(r) = rows.next()? {
            Ok(Some(HostInfo {
                os: r.get(0)?,
                os_version: r.get(1)?,
                arch: r.get(2)?,
                cpu_count: r.get(3)?,
                total_memory_mb: r.get(4)?,
                hostname_hash: r.get(5)?,
                machine_id_hash: r.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn metadata(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM _crab_metadata WHERE key = ?1")?;
        let mut rows = stmt.query(rusqlite::params![key])?;
        Ok(rows.next()?.map(|r| r.get::<_, String>(0)).transpose()?)
    }

    pub fn list_active_sessions(&self, limit: usize) -> Result<Vec<Session>> {
        session::list_active(&self.conn, limit)
    }

    pub fn list_recent_sessions(&self, limit: usize) -> Result<Vec<Session>> {
        session::list_recent(&self.conn, limit)
    }

    pub fn list_recent_events(
        &self,
        limit: usize,
        min_severity: Option<Severity>,
    ) -> Result<Vec<Event>> {
        event::list_recent(&self.conn, limit, min_severity)
    }

    /// Direct access to the underlying connection for the few
    /// callers (cleanup, watch, report) that need raw SQL.
    /// Intentionally `&Connection`, not `&mut`, to keep concurrent
    /// reads cheap.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn telemetry_enabled(&self) -> bool {
        self.telemetry_enabled
    }
}

fn default_db_path() -> Result<PathBuf> {
    if let Ok(home) = std::env::var("CRABCC_HOME") {
        return Ok(PathBuf::from(home).join("_internal.db"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow::anyhow!("$HOME unset; set CRABCC_HOME explicitly"))?;
    Ok(PathBuf::from(home).join(".crabcc").join("_internal.db"))
}

fn now_secs() -> u64 {
    crabcc_core::time::unix_now_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open_godfather() -> (Godfather, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("_internal.db");
        let g = Godfather::open_at(&path).unwrap();
        (g, dir)
    }

    #[test]
    fn install_record_is_idempotent() {
        let (g, _d) = open_godfather();
        g.record_install_once("3.0.0", InstallSource::Cargo)
            .unwrap();
        let first = g.metadata("install_time").unwrap().unwrap();
        // Second call must not change install_time.
        std::thread::sleep(std::time::Duration::from_secs(1));
        g.record_install_once("4.0.0", InstallSource::Source)
            .unwrap();
        let second = g.metadata("install_time").unwrap().unwrap();
        assert_eq!(first, second);
        // version is also locked to first writer.
        assert_eq!(g.metadata("install_version").unwrap().unwrap(), "3.0.0");
        assert_eq!(g.metadata("install_source").unwrap().unwrap(), "cargo");
    }

    #[test]
    fn host_info_upserts_on_each_open() {
        let (g, _d) = open_godfather();
        g.record_host_info().unwrap();
        let h1 = g.host_info().unwrap().unwrap();
        g.record_host_info().unwrap();
        let h2 = g.host_info().unwrap().unwrap();
        assert_eq!(h1, h2); // same machine, same boot — same hashes
    }

    #[test]
    fn session_lifecycle_round_trip() {
        let (g, _d) = open_godfather();
        let id = g.record_session_start("viz", "3.0.0", 1234).unwrap();
        assert_eq!(g.list_active_sessions(10).unwrap().len(), 1);
        g.record_session_end(&id, Some(0), None).unwrap();
        assert_eq!(g.list_active_sessions(10).unwrap().len(), 0);
        let s = g.list_recent_sessions(10).unwrap();
        assert_eq!(s[0].exit_code, Some(0));
    }

    #[test]
    fn telemetry_opt_out_makes_writes_no_op() {
        // Use the lower-level constructor so we don't poison the
        // shared process env (parallel tests would inherit it).
        let dir = tempdir().unwrap();
        let path = dir.path().join("_internal.db");
        let g = Godfather::open_at_with(&path, false).unwrap();
        assert!(!g.telemetry_enabled());

        g.record_install_once("3.0.0", InstallSource::Cargo)
            .unwrap();
        // No row landed — install_time is None.
        assert!(g.metadata("install_time").unwrap().is_none());
        let id = g.record_session_start("viz", "3.0", 1).unwrap();
        assert_eq!(id, "disabled");
        assert_eq!(g.list_active_sessions(10).unwrap().len(), 0);
    }

    #[test]
    fn record_event_then_list() {
        let (g, _d) = open_godfather();
        let id = g
            .record_event(None, Severity::Info, "godfather", "lifecycle", "open", None)
            .unwrap();
        assert!(id > 0);
        let evts = g.list_recent_events(10, None).unwrap();
        assert_eq!(evts.len(), 1);
    }

    #[test]
    fn batched_resource_samples_landed_atomically() {
        let (g, _d) = open_godfather();
        let sid = g.record_session_start("viz", "3.0", 4321).unwrap();
        let samples: Vec<ResourceSample> = (0u64..50)
            .map(|i| ResourceSample {
                ts: 1_700_000_000 + i,
                rss_mb: 100 + i,
                cpu_pct: 5.0 + i as f32,
                vsize_mb: 200 + i,
            })
            .collect();
        g.record_resource_samples(&sid, &samples).unwrap();

        let count: i64 = g
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM _crab_resource_sample WHERE session_id = ?1",
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 50);
    }

    #[test]
    fn batched_resource_samples_empty_slice_is_noop() {
        let (g, _d) = open_godfather();
        let sid = g.record_session_start("viz", "3.0", 1).unwrap();
        // Must not start a transaction for an empty batch — would
        // pay a write-lock cost for nothing.
        g.record_resource_samples(&sid, &[]).unwrap();
        let count: i64 = g
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM _crab_resource_sample WHERE session_id = ?1",
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn record_resource_sample_then_query_back() {
        let (g, _d) = open_godfather();
        let sid = g.record_session_start("viz", "3.0", 1).unwrap();
        for (rss, cpu, vsz) in [(100, 5.5, 200), (250, 12.0, 400), (180, 7.5, 300)] {
            g.record_resource_sample(&sid, rss, cpu, vsz).unwrap();
        }
        let (count, peak, _avg): (i64, i64, f64) = g
            .conn()
            .query_row(
                "SELECT COUNT(*), MAX(rss_mb), AVG(cpu_pct)
                 FROM _crab_resource_sample WHERE session_id = ?1",
                rusqlite::params![sid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(count, 3);
        assert_eq!(peak, 250);
    }

    #[test]
    fn record_crash_is_linked_to_session() {
        let (g, _d) = open_godfather();
        let sid = g.record_session_start("viz", "3.0", 4321).unwrap();
        let cid = g
            .record_crash(&sid, Some(139), Some(11), Some("…trailing log tail…"))
            .unwrap();
        assert!(cid > 0);
    }
}
