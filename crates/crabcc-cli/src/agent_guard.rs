//! `crabcc agent-guard` — periodic janitor for stuck / zombie agent runs.
//!
//! Designed to be invoked every 20 min by a LaunchAgent
//! (`com.crabcc.agent-guard.plist`). One sweep per invocation; the cadence
//! lives in launchd, not here. Returns 0 when the sweep ran cleanly even
//! if it killed runs — the caller cares whether the *guard* succeeded,
//! not whether any agents were misbehaving.
//!
//! Definitions:
//!   - **zombie** — row is `status = 'running'` but `kill(pid, 0)` says
//!     the PID is gone. The run died without finalizing (SIGKILL, OOM,
//!     reboot mid-run).
//!   - **stuck** — PID is alive but the run's log file hasn't been
//!     written to in `--idle-secs` (default 1800 = 30 min). The agent
//!     CLI is hung — could be a dead Anthropic socket, a deadlocked
//!     subprocess, or it's just genuinely thinking very hard. Kill at
//!     SIGTERM first, escalate to SIGKILL after 5 s.
//!
//! Side-effects:
//!   - Writes a per-run kill log at
//!     `~/.crabcc/agents/<id>/.agent-<id>-kill-log`. The dot-prefix keeps
//!     it from being matched by `tail -f log` glob patterns the user
//!     might have set up.
//!   - Records a row in `agent_kill_events` for each action taken.
//!   - Updates the `agent_runs` row to `status = 'crashed'`.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::agent_runs_db;

#[derive(Debug, Clone, Copy)]
pub struct GuardConfig {
    /// A run whose log file mtime is older than this is "stuck".
    pub idle_secs: u64,
    /// Hard deadline between SIGTERM and SIGKILL.
    pub term_grace_ms: u64,
    /// JSON output instead of the human table.
    pub json: bool,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            idle_secs: 1800,
            term_grace_ms: 5000,
            json: false,
        }
    }
}

#[derive(Debug)]
struct Action {
    run_id: String,
    pid: Option<i64>,
    reason: String,
    detail: String,
    log_path: Option<String>,
}

pub fn run(cfg: GuardConfig) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME not set"))?;
    let db_path = agent_runs_db::default_db_path(&home);
    if !db_path.exists() {
        // Nothing to guard — no install yet, or no agent has ever run.
        if cfg.json {
            println!(r#"{{"swept":0,"killed":0,"db":null}}"#);
        }
        return Ok(());
    }
    let conn = agent_runs_db::open(&db_path)?;

    // Reap obvious zombies first (cheap — PID-only check). reap_stale
    // marks them 'crashed' but doesn't write a kill event; do that here
    // so the audit trail is complete.
    let active = agent_runs_db::list_runs(&conn, true, 1024)?;
    let mut actions: Vec<Action> = Vec::new();
    for row in active {
        let pid = row.pid;
        let log_path = row.log_path.clone();
        let alive = pid_alive(pid);
        if !alive {
            actions.push(Action {
                run_id: row.id.clone(),
                pid,
                reason: "zombie".into(),
                detail: "PID not present in process table at sweep time".into(),
                log_path,
            });
            continue;
        }
        // PID alive — check for stuck-by-idle.
        if let Some(lp) = &row.log_path {
            if log_idle_secs(lp)
                .map(|s| s > cfg.idle_secs)
                .unwrap_or(false)
            {
                let detail = format!(
                    "log mtime older than idle_secs={} (path={})",
                    cfg.idle_secs, lp
                );
                #[cfg(unix)]
                if let Some(p) = pid {
                    let _ = unsafe { libc::kill(p as i32, libc::SIGTERM) };
                    std::thread::sleep(std::time::Duration::from_millis(cfg.term_grace_ms));
                    if pid_alive(Some(p)) {
                        let _ = unsafe { libc::kill(p as i32, libc::SIGKILL) };
                    }
                }
                actions.push(Action {
                    run_id: row.id.clone(),
                    pid,
                    reason: "stuck".into(),
                    detail,
                    log_path: row.log_path.clone(),
                });
            }
        }
    }

    // Persist + write per-run kill log files.
    for act in &actions {
        let _ = write_kill_log(&home, &act.run_id, act);
        let ev = agent_runs_db::KillEvent {
            run_id: act.run_id.clone(),
            reason: act.reason.clone(),
            pid: act.pid,
            log_path: act.log_path.clone(),
            detail: act.detail.clone(),
        };
        let _ = agent_runs_db::record_kill(&conn, &ev);
    }

    let killed = actions.len();
    if cfg.json {
        let arr = actions
            .iter()
            .map(|a| {
                format!(
                    r#"{{"run_id":"{}","reason":"{}","pid":{},"detail":"{}"}}"#,
                    a.run_id,
                    a.reason,
                    a.pid
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "null".into()),
                    a.detail.replace('"', "\\\"")
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        println!(
            r#"{{"swept":{},"killed":{},"actions":[{}]}}"#,
            killed, killed, arr
        );
    } else if killed > 0 {
        println!("crabcc agent-guard: {killed} action(s)");
        for a in &actions {
            println!("  {} :: {} :: {}", a.run_id, a.reason, a.detail);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn pid_alive(pid: Option<i64>) -> bool {
    match pid {
        Some(p) if p > 0 => (unsafe { libc::kill(p as i32, 0) }) == 0,
        _ => false,
    }
}

#[cfg(not(unix))]
fn pid_alive(_pid: Option<i64>) -> bool {
    true // pessimistic on non-unix — we don't run there yet
}

fn log_idle_secs(log_path: &str) -> Option<u64> {
    let attrs = std::fs::metadata(log_path).ok()?;
    let mtime = attrs.modified().ok()?;
    let now = SystemTime::now();
    now.duration_since(mtime).ok().map(|d| d.as_secs())
}

fn write_kill_log(home: &Path, run_id: &str, act: &Action) -> Result<()> {
    let dir = home.join(".crabcc").join("agents").join(run_id);
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join(format!(".agent-{run_id}-kill-log"));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = format!(
        "killed_at_unix: {now}\nreason: {}\npid: {}\ndetail: {}\n",
        act.reason,
        act.pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "n/a".into()),
        act.detail
    );
    std::fs::write(path, body)?;
    Ok(())
}
