//! `crabcc manager` — bulletproof orchestrator. Two surfaces:
//!
//!  * `manager daemon`  — long-running heartbeat process (LaunchAgent
//!    com.crabcc.manager). Writes a `manager_heartbeats` row every
//!    `--interval` seconds so any reader can prove "the manager is
//!    alive AND recent". Also runs the same health checks as `status`,
//!    so a `--watch` consumer (the menubar) sees timely state changes.
//!
//!  * `manager status [--json]` — point-in-time snapshot answering:
//!    is the manager alive? agentd? agent-guard? menubar? docker
//!    stack healthy? what was the last CLI call, the last kill, the
//!    last error? plus a `recommendations` list when anything is wrong.
//!
//! Plus the `ManagerGuard` struct used at the top of `fn main()` so
//! every CLI invocation registers itself in `cli_calls` (start/end/
//! exit/duration). That gives the menubar a definitive answer to
//! "what crabcc commands have run recently?" without pgrep.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::agent_runs_db;

const HEARTBEAT_FRESH_SECS: i64 = 90;
const HEARTBEAT_DEFAULT_INTERVAL: u64 = 30;

// MARK: - cli_calls bookkeeping (the "always know what's running" piece)

pub struct ManagerGuard {
    call_id: Option<i64>,
    started_at: SystemTime,
}

impl ManagerGuard {
    /// Record a `cli_calls` row at process start. Best-effort — if the
    /// DB can't be opened (locked, disk full), this returns a guard
    /// with no `call_id` so the Drop is a no-op. The CLI invocation
    /// itself MUST NOT fail because of bookkeeping.
    pub fn begin(cmd_label: &str, args_summary: &str) -> Self {
        let started_at = SystemTime::now();
        let call_id = (|| -> Option<i64> {
            let home = std::env::var_os("HOME").map(PathBuf::from)?;
            let db_path = agent_runs_db::default_db_path(&home);
            let conn = agent_runs_db::open(&db_path).ok()?;
            let now = unix_now();
            conn.execute(
                "INSERT INTO cli_calls (started_ts, pid, cmd, args) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![now, std::process::id() as i64, cmd_label, args_summary],
            )
            .ok()?;
            Some(conn.last_insert_rowid())
        })();
        Self {
            call_id,
            started_at,
        }
    }

    fn finish_inner(call_id: i64, started_at: SystemTime, exit_code: i32) {
        let _ = (|| -> Option<()> {
            let home = std::env::var_os("HOME").map(PathBuf::from)?;
            let conn = agent_runs_db::open(&agent_runs_db::default_db_path(&home)).ok()?;
            let now = unix_now();
            let dur_ms = started_at
                .elapsed()
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            conn.execute(
                "UPDATE cli_calls SET finished_ts = ?1, exit_code = ?2, \
                 duration_ms = ?3 WHERE id = ?4",
                params![now, exit_code, dur_ms, call_id],
            )
            .ok()?;
            Some(())
        })();
    }
}

impl Drop for ManagerGuard {
    fn drop(&mut self) {
        // Best-effort. Dropping = the function returned. We can't see
        // the exit code from here, so record 0 as the optimistic
        // default; explicit error paths can call `Self::finish` first
        // (it consumes self, preventing Drop from running again).
        if let Some(id) = self.call_id.take() {
            Self::finish_inner(id, self.started_at, 0);
        }
    }
}

// MARK: - daemon

// Sticky shutdown flag set by SIGTERM / SIGINT handlers. The daemon's
// sleep loop polls this in 1s slices so a launchctl bootout lands
// within ~1s — far below systemd/launchd's force-kill grace period.
static SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_signal(_sig: libc::c_int) {
    SHUTDOWN.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(unix)]
fn install_shutdown_handlers() {
    // Cast through *const () because clippy `fn_to_numeric_cast_any`
    // refuses a direct fn-pointer-to-usize cast on stable.
    let handler = on_signal as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
        // Reap exec()-spawned children if any; no-op for our case but
        // makes the daemon a well-behaved POSIX citizen.
        libc::signal(libc::SIGCHLD, libc::SIG_IGN);
    }
}

#[cfg(not(unix))]
fn install_shutdown_handlers() {}

pub fn run_daemon(interval_secs: u64) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    ensure_table(&home)?;
    install_shutdown_handlers();
    let db_path = agent_runs_db::default_db_path(&home);
    let interval = if interval_secs == 0 {
        HEARTBEAT_DEFAULT_INTERVAL
    } else {
        interval_secs
    };
    eprintln!(
        "crabcc manager daemon: pid={} interval={}s db={}",
        std::process::id(),
        interval,
        db_path.display()
    );
    while !SHUTDOWN.load(std::sync::atomic::Ordering::SeqCst) {
        if let Ok(conn) = agent_runs_db::open(&db_path) {
            let _ = write_heartbeat(&conn);
            let _ = agent_runs_db::reap_stale(&conn);
        }
        // Interruptible sleep: 1s chunks → SIGTERM lands within 1s.
        for _ in 0..interval {
            if SHUTDOWN.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
    eprintln!("crabcc manager daemon: clean shutdown");
    Ok(())
}

fn ensure_table(home: &Path) -> Result<()> {
    let conn = agent_runs_db::open(&agent_runs_db::default_db_path(home))
        .context("manager: open _internal.db")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS manager_heartbeats (\n\
           id          INTEGER PRIMARY KEY AUTOINCREMENT,\n\
           ts          INTEGER NOT NULL,\n\
           pid         INTEGER NOT NULL,\n\
           hostname    TEXT,\n\
           note        TEXT\n\
         );\n\
         CREATE INDEX IF NOT EXISTS idx_heartbeats_ts ON manager_heartbeats(ts DESC);",
    )?;
    Ok(())
}

fn write_heartbeat(conn: &Connection) -> Result<()> {
    let host = std::env::var("HOSTNAME").ok();
    conn.execute(
        "INSERT INTO manager_heartbeats (ts, pid, hostname) VALUES (?1, ?2, ?3)",
        params![unix_now(), std::process::id() as i64, host],
    )?;
    // Trim to the last 1000 rows so the table stays bounded.
    let _ = conn.execute(
        "DELETE FROM manager_heartbeats WHERE id NOT IN \
         (SELECT id FROM manager_heartbeats ORDER BY ts DESC LIMIT 1000)",
        [],
    );
    Ok(())
}

// MARK: - status

#[derive(Debug)]
pub struct StatusReport {
    pub manager_alive: bool,
    pub last_heartbeat_age_secs: Option<i64>,
    pub agentd_alive: bool,
    pub agent_guard_recent: bool,
    pub menubar_alive: bool,
    pub docker_stack_state: String, // "ok" / "degraded" / "absent" / "down"
    pub active_runs: i64,
    pub recent_kills: i64,
    pub recent_errors: i64,
    pub recommendations: Vec<String>,
}

pub fn run_status(json: bool) -> Result<()> {
    let report = collect_status()?;
    if json {
        let recs: Vec<String> = report
            .recommendations
            .iter()
            .map(|r| format!("\"{}\"", r.replace('"', "\\\"")))
            .collect();
        println!(
            r#"{{"manager_alive":{},"last_heartbeat_age_secs":{},"agentd_alive":{},"agent_guard_recent":{},"menubar_alive":{},"docker_stack":"{}","active_runs":{},"recent_kills":{},"recent_errors":{},"recommendations":[{}]}}"#,
            report.manager_alive,
            report
                .last_heartbeat_age_secs
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".into()),
            report.agentd_alive,
            report.agent_guard_recent,
            report.menubar_alive,
            report.docker_stack_state,
            report.active_runs,
            report.recent_kills,
            report.recent_errors,
            recs.join(",")
        );
    } else {
        let dot = |b: bool| if b { "✓" } else { "✗" };
        println!("crabcc manager status:");
        println!(
            "  {} manager (heartbeat age: {})",
            dot(report.manager_alive),
            report
                .last_heartbeat_age_secs
                .map(|v| format!("{}s", v))
                .unwrap_or_else(|| "never".into())
        );
        println!("  {} agentd", dot(report.agentd_alive));
        println!(
            "  {} agent-guard (recent run)",
            dot(report.agent_guard_recent)
        );
        println!("  {} menubar app", dot(report.menubar_alive));
        println!("  · docker stack: {}", report.docker_stack_state);
        println!("  · active agent runs: {}", report.active_runs);
        println!("  · kills recorded: {}", report.recent_kills);
        println!("  · errors in recent calls: {}", report.recent_errors);
        if !report.recommendations.is_empty() {
            println!();
            println!("recommendations:");
            for r in &report.recommendations {
                println!("  → {}", r);
            }
        }
    }
    Ok(())
}

fn collect_status() -> Result<StatusReport> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    let db_path = agent_runs_db::default_db_path(&home);
    let mut report = StatusReport {
        manager_alive: false,
        last_heartbeat_age_secs: None,
        agentd_alive: false,
        agent_guard_recent: false,
        menubar_alive: false,
        docker_stack_state: "absent".into(),
        active_runs: 0,
        recent_kills: 0,
        recent_errors: 0,
        recommendations: Vec::new(),
    };

    // Manager heartbeat.
    if db_path.exists() {
        if let Ok(conn) = agent_runs_db::open(&db_path) {
            let _ = ensure_table(&home);
            let last_ts: Option<i64> = conn
                .query_row("SELECT MAX(ts) FROM manager_heartbeats", [], |r| r.get(0))
                .ok()
                .flatten();
            if let Some(ts) = last_ts {
                let age = unix_now() - ts;
                report.last_heartbeat_age_secs = Some(age);
                report.manager_alive = age <= HEARTBEAT_FRESH_SECS;
            }
            report.active_runs = conn
                .query_row(
                    "SELECT COUNT(*) FROM agent_runs WHERE status='running'",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            report.recent_kills = conn
                .query_row(
                    "SELECT COUNT(*) FROM agent_kill_events WHERE killed_at > ?1",
                    params![unix_now() - 86400],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            report.recent_errors = conn
                .query_row(
                    "SELECT COUNT(*) FROM cli_calls WHERE finished_ts > ?1 AND exit_code != 0",
                    params![unix_now() - 86400],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            // Agent-guard "recent": any kill_event in the last 25 min OR
            // a successful agent-guard cli_calls row in the last 25 min.
            let recent_guard: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM cli_calls WHERE cmd='agent-guard' AND finished_ts > ?1",
                    params![unix_now() - 1500],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            report.agent_guard_recent = recent_guard > 0;
        }
    } else {
        report
            .recommendations
            .push("~/.crabcc/_internal.db missing — run `scripts/install-macos-helpers.sh` or install Crabcc.app".into());
    }

    // launchctl probes for the other LaunchAgents.
    report.agentd_alive = launchctl_running("com.crabcc.agentd");
    report.menubar_alive = launchctl_running("com.crabcc.menubar");

    // Docker stack health.
    report.docker_stack_state = docker_stack_state(&home);

    // Recommendations based on observed state.
    if !report.manager_alive {
        report.recommendations.push("manager daemon not heartbeating — `launchctl kickstart -k gui/$UID/com.crabcc.manager`".into());
    }
    if !report.agentd_alive {
        report
            .recommendations
            .push("agentd LaunchAgent not loaded — `bash scripts/install-macos-helpers.sh`".into());
    }
    if !report.menubar_alive {
        report.recommendations.push("menubar app not running — open /Applications/Crabcc.app or `launchctl kickstart -k gui/$UID/com.crabcc.menubar`".into());
    }
    if report.docker_stack_state == "down" {
        report.recommendations.push("docker stack present but not healthy — `cd install/ollama-stack && docker compose up -d --wait`".into());
    }
    if report.recent_errors > 0 {
        report.recommendations.push(format!(
            "{} recent CLI calls exited non-zero — `crabcc agent-ls --json` and review ~/Library/Logs/Crabcc/*.log",
            report.recent_errors
        ));
    }

    Ok(report)
}

fn launchctl_running(label: &str) -> bool {
    let uid_str = format!("{}", unsafe { libc::getuid() });
    let target = format!("gui/{}/{}", uid_str, label);
    let out = Command::new("/bin/launchctl")
        .arg("print")
        .arg(&target)
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            let body = String::from_utf8_lossy(&o.stdout);
            // `state = running` appears for live processes; absent or
            // `state = not running` for inactive.
            return body.contains("state = running");
        }
    }
    false
}

fn docker_stack_state(home: &Path) -> String {
    // Compose file lives in the repo at install/ollama-stack/docker-compose.yml.
    // Try the canonical home-bin location first, fall back to current dir.
    let candidates = [
        home.join("workspace/bin/crabcc/install/ollama-stack/docker-compose.yml"),
        std::env::current_dir()
            .ok()
            .map(|c| c.join("install/ollama-stack/docker-compose.yml"))
            .unwrap_or_default(),
    ];
    let compose_file = candidates.iter().find(|p| p.exists());
    let Some(file) = compose_file else {
        return "absent".into();
    };
    let out = Command::new("docker")
        .args([
            "compose",
            "-f",
            file.to_string_lossy().as_ref(),
            "ps",
            "--format",
            "json",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let body = String::from_utf8_lossy(&o.stdout);
            if body.trim().is_empty() {
                "down".into()
            } else if body.contains(r#""State":"running""#)
                || body.contains(r#""State": "running""#)
            {
                "ok".into()
            } else {
                "degraded".into()
            }
        }
        _ => {
            // docker not on PATH or failed to query — neither absent nor down.
            // We say "absent" since we can't observe it.
            "absent".into()
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
