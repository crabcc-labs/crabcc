//! Singleton SQLite store for `crabcc agent` runs at
//! `~/.crabcc/_internal.db`. Tracks every run's PID, repo, timestamps,
//! exit code, and the on-disk paths to its log + meta — so the menubar
//! app + `crabcc agent ls` can answer "what's running right now" without
//! pgrep heuristics or filesystem walks.
//!
//! Schema (additive — never DROP COLUMN, mirror `Store::open` rules):
//!
//! ```text
//! agent_runs(
//!   id          TEXT PK,
//!   started_ts  INTEGER NOT NULL,
//!   finished_ts INTEGER NULL,
//!   pid         INTEGER NULL,
//!   repo        TEXT NOT NULL,
//!   runtime     TEXT,
//!   model       TEXT,
//!   log_path    TEXT,
//!   meta_path   TEXT,
//!   exit_code   INTEGER,
//!   status      TEXT NOT NULL DEFAULT 'running'  -- 'running' | 'finished' | 'crashed'
//! )
//! ```
//!
//! All writes are best-effort — if the DB is locked or unwritable we log
//! at `debug` and continue. The agent run itself must never fail because
//! of bookkeeping.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

/// Default DB location: `~/.crabcc/_internal.db`. Singleton across all
/// repos for the current user — there is exactly one DB.
pub fn default_db_path(home: &Path) -> PathBuf {
    home.join(".crabcc").join("_internal.db")
}

/// Open + migrate the DB. Idempotent; safe to call from any code path
/// since `CREATE TABLE IF NOT EXISTS` handles fresh installs.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let conn =
        Connection::open(path).with_context(|| format!("open agent runs db {}", path.display()))?;
    // Sequoia-tolerant pragmas: WAL keeps reader (menubar) + writer
    // (agent.rs) from blocking each other.
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;\n\
         PRAGMA synchronous  = NORMAL;\n\
         CREATE TABLE IF NOT EXISTS agent_runs (\n\
           id           TEXT PRIMARY KEY,\n\
           started_ts   INTEGER NOT NULL,\n\
           finished_ts  INTEGER,\n\
           pid          INTEGER,\n\
           repo         TEXT NOT NULL,\n\
           runtime      TEXT,\n\
           model        TEXT,\n\
           log_path     TEXT,\n\
           meta_path    TEXT,\n\
           exit_code    INTEGER,\n\
           status       TEXT NOT NULL DEFAULT 'running'\n\
         );\n\
         CREATE INDEX IF NOT EXISTS idx_agent_runs_started ON agent_runs(started_ts DESC);\n\
         CREATE INDEX IF NOT EXISTS idx_agent_runs_status  ON agent_runs(status);\n\
         CREATE TABLE IF NOT EXISTS agent_kill_events (\n\
           id           INTEGER PRIMARY KEY AUTOINCREMENT,\n\
           run_id       TEXT NOT NULL,\n\
           killed_at    INTEGER NOT NULL,\n\
           reason       TEXT NOT NULL,  -- 'stuck' | 'zombie' | 'manual'\n\
           pid          INTEGER,\n\
           log_path     TEXT,\n\
           detail       TEXT\n\
         );\n\
         CREATE INDEX IF NOT EXISTS idx_kill_events_run    ON agent_kill_events(run_id);\n\
         CREATE INDEX IF NOT EXISTS idx_kill_events_at     ON agent_kill_events(killed_at DESC);\n\
         CREATE TABLE IF NOT EXISTS backup_runs (\n\
           id           INTEGER PRIMARY KEY AUTOINCREMENT,\n\
           ran_at       INTEGER NOT NULL,\n\
           repo         TEXT NOT NULL,\n\
           destination  TEXT NOT NULL,\n\
           files        INTEGER NOT NULL,\n\
           dirs         INTEGER NOT NULL,\n\
           bytes        INTEGER NOT NULL,\n\
           pruned       INTEGER NOT NULL,\n\
           trigger      TEXT NOT NULL DEFAULT 'manual'\n\
         );\n\
         CREATE INDEX IF NOT EXISTS idx_backup_runs_at   ON backup_runs(ran_at DESC);\n\
         CREATE INDEX IF NOT EXISTS idx_backup_runs_repo ON backup_runs(repo);",
    )?;
    Ok(conn)
}

/// Append a row to `backup_runs`. Best-effort; caller is expected to
/// ignore errors so a transient DB lock never breaks the snapshot path.
#[allow(clippy::too_many_arguments)]
pub fn record_backup(
    conn: &Connection,
    repo: &str,
    destination: &str,
    files: i64,
    dirs: i64,
    bytes: i64,
    pruned: i64,
    trigger: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO backup_runs (ran_at, repo, destination, files, dirs, bytes, pruned, trigger) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            now_ts(),
            repo,
            destination,
            files,
            dirs,
            bytes,
            pruned,
            trigger
        ],
    )?;
    Ok(())
}

#[derive(Debug)]
pub struct KillEvent {
    pub run_id: String,
    pub reason: String,
    pub pid: Option<i64>,
    pub log_path: Option<String>,
    pub detail: String,
}

pub fn record_kill(conn: &Connection, ev: &KillEvent) -> Result<()> {
    conn.execute(
        "INSERT INTO agent_kill_events (run_id, killed_at, reason, pid, log_path, detail) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            ev.run_id,
            now_ts(),
            ev.reason,
            ev.pid,
            ev.log_path,
            ev.detail,
        ],
    )?;
    conn.execute(
        "UPDATE agent_runs SET status = 'crashed', finished_ts = COALESCE(finished_ts, ?1), \
                exit_code = COALESCE(exit_code, -9) \
         WHERE id = ?2",
        params![now_ts(), ev.run_id],
    )?;
    Ok(())
}

pub fn list_kill_events(conn: &Connection, limit: usize) -> Result<Vec<KillEvent>> {
    let sql = format!(
        "SELECT run_id, reason, pid, log_path, detail \
         FROM agent_kill_events ORDER BY killed_at DESC LIMIT {limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(KillEvent {
                run_id: r.get(0)?,
                reason: r.get(1)?,
                pid: r.get(2)?,
                log_path: r.get(3)?,
                detail: r.get(4)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

fn now_ts() -> i64 {
    crabcc_core::time::unix_now_secs() as i64
}

/// Insert a new run row at lifecycle start. PID can be filled in later
/// via `update_pid` once the child has actually spawned.
pub fn insert_run(
    conn: &Connection,
    id: &str,
    repo: &Path,
    runtime: &str,
    model: Option<&str>,
    log_path: &Path,
    meta_path: &Path,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO agent_runs \
           (id, started_ts, repo, runtime, model, log_path, meta_path, status) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running')",
        params![
            id,
            now_ts(),
            repo.display().to_string(),
            runtime,
            model,
            log_path.display().to_string(),
            meta_path.display().to_string(),
        ],
    )?;
    Ok(())
}

pub fn update_pid(conn: &Connection, id: &str, pid: u32) -> Result<()> {
    conn.execute(
        "UPDATE agent_runs SET pid = ?1 WHERE id = ?2",
        params![pid, id],
    )?;
    Ok(())
}

pub fn mark_finished(conn: &Connection, id: &str, exit_code: i32) -> Result<()> {
    let status = if exit_code == 0 {
        "finished"
    } else {
        "crashed"
    };
    conn.execute(
        "UPDATE agent_runs \
         SET finished_ts = ?1, exit_code = ?2, status = ?3 \
         WHERE id = ?4",
        params![now_ts(), exit_code, status, id],
    )?;
    Ok(())
}

/// Reap any rows still marked `running` whose PID no longer exists.
/// Called on process startup so menubar counts don't get pinned at "1
/// running" forever after a hard kill. POSIX-only — on non-unix we
/// no-op (we don't ship there yet).
#[cfg(unix)]
pub fn reap_stale(conn: &Connection) -> Result<usize> {
    let mut stmt = conn.prepare("SELECT id, pid FROM agent_runs WHERE status = 'running'")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();
    let mut reaped = 0;
    for (id, pid) in rows {
        let dead = match pid {
            Some(p) if p > 0 => (unsafe { libc::kill(p as i32, 0) }) != 0,
            _ => true, // null pid → assume dead
        };
        if dead {
            conn.execute(
                "UPDATE agent_runs SET status = 'crashed' WHERE id = ?1 AND status = 'running'",
                params![id],
            )?;
            reaped += 1;
        }
    }
    Ok(reaped)
}

#[cfg(not(unix))]
pub fn reap_stale(_conn: &Connection) -> Result<usize> {
    Ok(0)
}

/// JSON-serializable snapshot row used by `crabcc agent ls --json`.
#[derive(Debug)]
pub struct RunRow {
    pub id: String,
    pub started_ts: i64,
    pub finished_ts: Option<i64>,
    pub pid: Option<i64>,
    pub repo: String,
    pub runtime: Option<String>,
    pub model: Option<String>,
    pub log_path: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
}

pub fn list_runs(conn: &Connection, only_active: bool, limit: usize) -> Result<Vec<RunRow>> {
    let where_clause = if only_active {
        "WHERE status = 'running'"
    } else {
        ""
    };
    let sql = format!(
        "SELECT id, started_ts, finished_ts, pid, repo, runtime, model, log_path, status, exit_code \
         FROM agent_runs {where_clause} \
         ORDER BY started_ts DESC LIMIT {limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(RunRow {
                id: r.get(0)?,
                started_ts: r.get(1)?,
                finished_ts: r.get(2)?,
                pid: r.get(3)?,
                repo: r.get(4)?,
                runtime: r.get(5)?,
                model: r.get(6)?,
                log_path: r.get(7)?,
                status: r.get(8)?,
                exit_code: r.get(9)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn insert_and_finish_round_trip() {
        let home = tempdir().unwrap();
        let db = default_db_path(home.path());
        let conn = open(&db).unwrap();
        insert_run(
            &conn,
            "abc",
            Path::new("/repo"),
            "subprocess",
            Some("opus"),
            Path::new("/log"),
            Path::new("/meta"),
        )
        .unwrap();
        update_pid(&conn, "abc", 4242).unwrap();
        let active = list_runs(&conn, true, 10).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].pid, Some(4242));
        mark_finished(&conn, "abc", 0).unwrap();
        let still_active = list_runs(&conn, true, 10).unwrap();
        assert!(still_active.is_empty());
        let all = list_runs(&conn, false, 10).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, "finished");
    }
}
