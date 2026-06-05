//! `crabcc agent-guard` — periodic janitor for stuck / zombie agent runs.
//!
//! Designed to be invoked every 20 min by an external scheduler (cron /
//! launchd / systemd). One sweep per invocation; the cadence lives in the
//! scheduler, not here. Returns 0 when the sweep ran cleanly even
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
    // Worst case: every active run requires an action. Hint at the
    // upper bound so the inner push loop avoids the 4 → 8 → 16 …
    // doubling chain even when sweep counts spike.
    let mut actions: Vec<Action> = Vec::with_capacity(active.len());
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
                .unwrap_or_default()
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
        .unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_config_default_values() {
        let cfg = GuardConfig::default();
        assert_eq!(cfg.idle_secs, 1800);
        assert_eq!(cfg.term_grace_ms, 5000);
        assert!(!cfg.json);
    }

    #[test]
    fn pid_alive_none_returns_false() {
        assert!(!pid_alive(None));
    }

    #[test]
    fn pid_alive_zero_returns_false() {
        assert!(!pid_alive(Some(0)));
    }

    #[test]
    fn pid_alive_negative_returns_false() {
        assert!(!pid_alive(Some(-1)));
    }

    #[cfg(unix)]
    #[test]
    fn pid_alive_current_process() {
        let pid = std::process::id() as i64;
        assert!(pid_alive(Some(pid)));
    }

    #[cfg(unix)]
    #[test]
    fn pid_alive_nonexistent_pid() {
        // PID 2^30 is very unlikely to exist
        assert!(!pid_alive(Some(1_073_741_824)));
    }

    #[test]
    fn log_idle_secs_nonexistent_file() {
        assert_eq!(log_idle_secs("/nonexistent/path/to/logfile.log"), None);
    }

    #[test]
    fn log_idle_secs_fresh_file_is_small() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("test.log");
        std::fs::write(&log, "some log content").unwrap();
        let idle = log_idle_secs(log.to_str().unwrap());
        // A file just written should have idle < 5 seconds
        assert!(idle.is_some());
        assert!(
            idle.unwrap() < 5,
            "expected < 5s idle, got {}",
            idle.unwrap()
        );
    }

    #[test]
    fn write_kill_log_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let act = Action {
            run_id: "test-run-123".to_string(),
            pid: Some(12345),
            reason: "zombie".to_string(),
            detail: "PID gone".to_string(),
            log_path: None,
        };
        write_kill_log(home, "test-run-123", &act).unwrap();

        let expected = home
            .join(".crabcc")
            .join("agents")
            .join("test-run-123")
            .join(".agent-test-run-123-kill-log");
        assert!(expected.exists());
        let content = std::fs::read_to_string(expected).unwrap();
        assert!(content.contains("reason: zombie"));
        assert!(content.contains("pid: 12345"));
        assert!(content.contains("detail: PID gone"));
    }

    #[test]
    fn write_kill_log_no_pid() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let act = Action {
            run_id: "run-no-pid".to_string(),
            pid: None,
            reason: "stuck".to_string(),
            detail: "log stale".to_string(),
            log_path: Some("/tmp/fake.log".to_string()),
        };
        write_kill_log(home, "run-no-pid", &act).unwrap();

        let path = home
            .join(".crabcc")
            .join("agents")
            .join("run-no-pid")
            .join(".agent-run-no-pid-kill-log");
        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("pid: n/a"));
    }

    #[test]
    fn run_with_no_db_exits_cleanly() {
        // When HOME points to an empty dir (no _internal.db), agent-guard
        // should exit successfully with no actions.
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", dir.path());
        let cfg = GuardConfig {
            json: true,
            ..Default::default()
        };
        let result = run(cfg);
        assert!(result.is_ok());
        // Restore HOME isn't strictly needed in tests but good practice
    }
}
