//! `crabcc run -- <command>` — run a command, capture its output to a per-run
//! log, and pipe what's shown through [`crate::squeeze`].
//!
//! The design point: a long / blocking command (build, test, server, `tail -f`)
//! is **detached to the background, never killed**. The child runs in its own
//! session (`setsid`) writing straight to `~/.crabcc/runs/<id>/log` — a file it
//! owns, independent of this process — so when `run` returns to the agent the
//! command keeps going and nothing is lost. The agent gets an instant squeezed
//! view of the output so far plus how to follow or stop it.
//!
//! - finishes before the idle/total threshold  -> squeeze full output, exit code.
//! - hits `--idle`/`--timeout` (or `--bg`)      -> detach, return instantly with
//!   a `run <id>` handle (the command keeps running).
//! - `--follow <id>` -> a non-blocking squeezed snapshot of a run's log + status.
//! - `--list` / `--kill <id>` -> manage background runs.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Serialize, Deserialize)]
struct Meta {
    id: String,
    cmd: String,
    pid: u32,
    started_at: u64,
    #[serde(default)]
    session_id: String,
}

/// Dispatch the `crabcc run` flag surface. Exactly one of start / follow / list
/// / kill runs based on which flags are set.
#[allow(clippy::too_many_arguments)]
pub fn run(
    cmd: &[String],
    max_lines: usize,
    idle: u64,
    timeout: u64,
    max_bytes: usize,
    bg: bool,
    follow: Option<&str>,
    kill: Option<&str>,
    list: bool,
) -> Result<()> {
    prune_old_runs();
    if list {
        return list_runs();
    }
    if let Some(id) = kill {
        return kill_run(id);
    }
    if let Some(id) = follow {
        return follow_run(id, max_lines, max_bytes);
    }
    start(cmd, max_lines, idle, timeout, max_bytes, bg)
}

fn start(
    cmd: &[String],
    max_lines: usize,
    idle: u64,
    timeout: u64,
    max_bytes: usize,
    bg: bool,
) -> Result<()> {
    if cmd.is_empty() {
        bail!("usage: crabcc run [--bg] [--idle S] [--timeout S] -- <command>");
    }
    let joined = cmd.join(" ");
    let id = new_id();
    let dir = runs_dir().join(&id);
    std::fs::create_dir_all(&dir).with_context(|| format!("create run dir {}", dir.display()))?;
    let log_path = dir.join("log");
    let log = File::create(&log_path).with_context(|| format!("create {}", log_path.display()))?;
    let log_err = log.try_clone()?;

    // Exec the command directly (no wrapping shell), so arg boundaries are
    // preserved exactly — `crabcc run -- sh -c '...'` then runs the user's
    // shell with the script as one intact arg, and there's no join/quoting
    // mangling. Child writes stdout+stderr straight to the log file (a file it
    // owns, not a pipe we hold), in its own session, so it survives this
    // process detaching/exiting.
    let mut command = Command::new(&cmd[0]);
    command
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        // New session: the child leads its own session+group, detached from the
        // controlling terminal, so it keeps running after `run` returns.
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("spawn `{joined}`"))?;
    let pid = child.id();

    write_meta(
        &dir,
        &Meta {
            id: id.clone(),
            cmd: joined.clone(),
            pid,
            started_at: now_secs(),
            session_id: std::env::var("CRABCC_SESSION_ID").unwrap_or_default(),
        },
    )?;

    // Tail the log until the child exits or a threshold/`--bg` says to detach.
    let mut rf = File::open(&log_path)?;
    let mut captured: Vec<u8> = Vec::new();
    let start = Instant::now();
    let mut last = Instant::now();
    let mut chunk = [0u8; 8192];
    loop {
        let n = rf.read(&mut chunk).unwrap_or(0);
        if n > 0 {
            last = Instant::now();
            append_capped(&mut captured, &chunk[..n], max_bytes);
            continue;
        }
        if let Some(status) = child.try_wait()? {
            // Drain anything written between the last read and exit.
            loop {
                let n = rf.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                append_capped(&mut captured, &chunk[..n], max_bytes);
            }
            return finish(
                &captured,
                max_lines,
                &joined,
                status.code(),
                &log_path,
                start,
            );
        }
        let now = Instant::now();
        let hit = bg
            || (timeout > 0 && now.duration_since(start) >= Duration::from_secs(timeout))
            || (idle > 0 && now.duration_since(last) >= Duration::from_secs(idle));
        if hit {
            // Detach: do NOT kill or wait. Dropping `child` does not kill it
            // (std), and it owns the log file, so it keeps running + writing.
            return detach(&captured, max_lines, &id, pid, &joined, &log_path, start);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn finish(
    captured: &[u8],
    max_lines: usize,
    joined: &str,
    code: Option<i32>,
    log_path: &Path,
    start: Instant,
) -> Result<()> {
    let text = String::from_utf8_lossy(captured);
    let (out, stats) = crate::squeeze::squeeze(&text, max_lines);
    print!("{out}");
    eprintln!("{}", crate::squeeze::disclosure(&stats));
    let code = code.unwrap_or(-1);
    eprintln!(
        "[crabcc run] `{joined}` exited {code} in {:.1}s; log: {}",
        start.elapsed().as_secs_f64(),
        log_path.display()
    );
    std::process::exit(code);
}

#[allow(clippy::too_many_arguments)]
fn detach(
    captured: &[u8],
    max_lines: usize,
    id: &str,
    pid: u32,
    joined: &str,
    log_path: &Path,
    start: Instant,
) -> Result<()> {
    let text = String::from_utf8_lossy(captured);
    let (out, stats) = crate::squeeze::squeeze(&text, max_lines);
    print!("{out}");
    eprintln!("{}", crate::squeeze::disclosure(&stats));
    eprintln!(
        "[crabcc run] `{joined}` STILL RUNNING in the background as run {id} (pid {pid}) after \
         {:.1}s — output above is a snapshot, not the final result. \
         Follow: `crabcc run --follow {id}`  |  stop: `crabcc run --kill {id}`  |  log: {}",
        start.elapsed().as_secs_f64(),
        log_path.display()
    );
    Ok(())
}

fn follow_run(id: &str, max_lines: usize, max_bytes: usize) -> Result<()> {
    if id.is_empty() {
        bail!("--follow needs a run id (see `crabcc run --list`)");
    }
    let dir = runs_dir().join(id);
    let meta = read_meta(&dir).with_context(|| format!("no run {id} (try `crabcc run --list`)"))?;
    let log_path = dir.join("log");
    let mut captured = Vec::new();
    if let Ok(mut f) = File::open(&log_path) {
        let mut chunk = [0u8; 8192];
        while let Ok(n) = f.read(&mut chunk) {
            if n == 0 {
                break;
            }
            append_capped(&mut captured, &chunk[..n], max_bytes);
        }
    }
    let text = String::from_utf8_lossy(&captured);
    let (out, stats) = crate::squeeze::squeeze(&text, max_lines);
    print!("{out}");
    eprintln!("{}", crate::squeeze::disclosure(&stats));
    if alive(meta.pid) {
        eprintln!(
            "[crabcc run] run {id} STILL RUNNING (pid {}) — snapshot above; re-run \
             `crabcc run --follow {id}` for more, or `crabcc run --kill {id}` to stop. cmd: `{}`",
            meta.pid, meta.cmd
        );
    } else {
        eprintln!(
            "[crabcc run] run {id} has FINISHED — output above is complete. cmd: `{}`",
            meta.cmd
        );
    }
    Ok(())
}

fn list_runs() -> Result<()> {
    let root = runs_dir();
    let mut rows: Vec<Meta> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.flatten() {
            if let Ok(m) = read_meta(&e.path()) {
                rows.push(m);
            }
        }
    }
    rows.sort_by_key(|m| std::cmp::Reverse(m.started_at));
    if rows.is_empty() {
        println!("(no runs)");
        return Ok(());
    }
    let (c_id, c_pid, c_state, c_cmd) = ("id", "pid", "state", "cmd");
    println!("{c_id:<20}  {c_pid:<7}  {c_state:<8}  {c_cmd}");
    for m in rows {
        let state = if alive(m.pid) { "running" } else { "done" };
        println!("{:<20}  {:<7}  {:<8}  {}", m.id, m.pid, state, m.cmd);
    }
    Ok(())
}

fn kill_run(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("--kill needs a run id (see `crabcc run --list`)");
    }
    let dir = runs_dir().join(id);
    let meta = read_meta(&dir).with_context(|| format!("no run {id}"))?;
    if !alive(meta.pid) {
        eprintln!("[crabcc run] run {id} already finished (pid {})", meta.pid);
        return Ok(());
    }
    kill_group(meta.pid);
    eprintln!(
        "[crabcc run] killed run {id} (pid {}, `{}`)",
        meta.pid, meta.cmd
    );
    Ok(())
}

// ── helpers ────────────────────────────────────────────────────────────────

fn append_capped(buf: &mut Vec<u8>, bytes: &[u8], max_bytes: usize) {
    if buf.len() >= max_bytes {
        return;
    }
    let take = (max_bytes - buf.len()).min(bytes.len());
    buf.extend_from_slice(&bytes[..take]);
}

fn runs_dir() -> PathBuf {
    let home = std::env::var_os("CRABCC_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".crabcc")))
        .unwrap_or_else(|| PathBuf::from(".crabcc"));
    home.join("runs")
}

fn new_id() -> String {
    // sortable timestamp + a little entropy from the pid + nanos.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "{}-{:04x}",
        now.as_secs(),
        (now.subsec_nanos() ^ std::process::id()) & 0xffff
    )
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn write_meta(dir: &Path, m: &Meta) -> Result<()> {
    let s = serde_json::to_string(m)?;
    std::fs::write(dir.join("meta.json"), s).context("write run meta")
}

fn read_meta(dir: &Path) -> Result<Meta> {
    let s = std::fs::read_to_string(dir.join("meta.json"))
        .map_err(|_| anyhow!("no meta in {}", dir.display()))?;
    Ok(serde_json::from_str(&s)?)
}

/// True if `pid` is a live (non-zombie) process. `kill(pid, 0)` checks
/// existence, but a killed-but-unreaped zombie still "exists" — on Linux we
/// read `/proc/<pid>/stat` and treat state `Z` as dead so a just-killed run
/// reads as finished.
#[cfg(target_os = "linux")]
fn alive(pid: u32) -> bool {
    if pid == 0 || unsafe { libc::kill(pid as i32, 0) } != 0 {
        return false;
    }
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // state is the first token after the `)` that closes `(comm)`.
        Ok(s) => s
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .map(|st| st != "Z")
            .unwrap_or(true),
        Err(_) => false,
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn alive(pid: u32) -> bool {
    pid != 0 && unsafe { libc::kill(pid as i32, 0) } == 0
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    false
}

/// Kill the whole session/group led by `pid` (we `setsid` the child, so its
/// pid is its group id).
fn kill_group(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

/// Best-effort prune of finished run dirs older than 24h, so detached runs
/// don't accumulate. Live runs are always kept.
fn prune_old_runs() {
    let root = runs_dir();
    let cutoff = now_secs().saturating_sub(24 * 3600);
    let Ok(rd) = std::fs::read_dir(&root) else {
        return;
    };
    for e in rd.flatten() {
        if let Ok(m) = read_meta(&e.path()) {
            if m.started_at < cutoff && !alive(m.pid) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_capped_respects_the_cap() {
        let mut b = Vec::new();
        append_capped(&mut b, b"hello", 3);
        assert_eq!(b, b"hel");
        append_capped(&mut b, b"world", 3); // already full
        assert_eq!(b, b"hel");
    }

    #[test]
    fn new_id_is_unique_and_sortable() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert!(a.contains('-'));
    }
}
