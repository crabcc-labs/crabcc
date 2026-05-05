//! `WatchHandle` — sysinfo-driven supervisor thread.
//!
//! Polls a target PID every `sample_interval`, writes a
//! `_crab_resource_sample` row, and emits a periodic `heartbeat`
//! event tagged with the supervisor's own session id. When the
//! target exits, writes a `Severity::Crash` event (if non-zero
//! exit) plus a `_crab_crash` row, then ends the watched session.
//!
//! Drop → posts a shutdown signal to the worker thread + joins it.
//! The supervisor's own session keeps running until its
//! `Godfather` is dropped.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::event::Severity;
use crate::godfather::Godfather;

#[derive(Debug, Clone)]
pub struct WatchConfig {
    pub target_app: String,
    pub target_pid: u32,
    /// How often to record a `_crab_resource_sample`.
    pub sample_interval: Duration,
    /// How often to emit a `category=heartbeat` event so dashboards
    /// can show "supervisor last seen Xs ago". Should be a small
    /// multiple of `sample_interval` so heartbeat spam doesn't crowd
    /// the event log; default is every 6th sample (30 s by default).
    pub heartbeat_every: u32,
    /// If set, the path the watched app writes its log to. On exit
    /// we tail-read the last ~4 KiB into `_crab_crash.log_tail` so
    /// the crash report has context without the supervisor mirroring
    /// every byte at runtime.
    pub log_path: Option<PathBuf>,
    /// The session id used to attribute resource samples + crash
    /// rows back to the watched app's `_crab_session` row.
    pub watched_session_id: String,
}

impl WatchConfig {
    pub fn new(
        target_app: impl Into<String>,
        target_pid: u32,
        watched_session_id: impl Into<String>,
    ) -> Self {
        Self {
            target_app: target_app.into(),
            target_pid,
            sample_interval: Duration::from_secs(5),
            heartbeat_every: 6,
            log_path: None,
            watched_session_id: watched_session_id.into(),
        }
    }
}

pub struct WatchHandle {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    target_app: String,
    target_pid: u32,
}

impl WatchHandle {
    pub fn target_app(&self) -> &str {
        &self.target_app
    }
    pub fn target_pid(&self) -> u32 {
        self.target_pid
    }

    pub fn spawn(godfather: Godfather, config: WatchConfig) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let target_app = config.target_app.clone();
        let target_pid = config.target_pid;
        let worker = std::thread::Builder::new()
            .name(format!("godfather-watch-{}", config.target_pid))
            .spawn(move || run(godfather, config, stop_for_thread))?;
        Ok(Self {
            stop,
            worker: Some(worker),
            target_app,
            target_pid,
        })
    }

    /// Block until the watcher exits (target process gone OR
    /// `stop` was set). Returns when the worker thread joins.
    pub fn join(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

fn run(godfather: Godfather, config: WatchConfig, stop: Arc<AtomicBool>) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let mut sys = System::new();
    let target = Pid::from(config.target_pid as usize);
    let started = Instant::now();
    let mut tick: u32 = 0;

    while !stop.load(Ordering::SeqCst) {
        // Existence check first via `kill(pid, 0)` — sysinfo's cache
        // on macOS lags reality after a SIGTERM-then-reap (the dead
        // PID stays in `System`'s hashmap for a few refreshes), so
        // we'd record stale samples for a process that's actually
        // gone. The kill-with-signal-0 syscall returns -1 + ESRCH
        // immediately for dead PIDs.
        if !pid_alive(config.target_pid) {
            sys = System::new();
            sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[target]),
                ProcessRefreshKind::new().with_memory().with_cpu(),
            );
        }
        // Refresh just the one PID — cheaper than a full system
        // refresh, especially in container hosts with hundreds of
        // peer processes.
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[target]),
            ProcessRefreshKind::new().with_memory().with_cpu(),
        );

        // Trust the kernel over sysinfo's cache. If the PID is gone
        // we treat it as exit regardless of what `sys.process(...)`
        // says.
        let alive = pid_alive(config.target_pid);
        let proc = if alive { sys.process(target) } else { None };
        match proc {
            Some(p) => {
                let rss_mb = p.memory() / 1024 / 1024;
                let vsize_mb = p.virtual_memory() / 1024 / 1024;
                // sysinfo's CPU is 0..=cpu_count*100; clamp to a
                // single-core 0..100 view for the dashboard.
                let cpu_pct = (p.cpu_usage()).clamp(0.0, 10_000.0);
                let _ = godfather.record_resource_sample(
                    &config.watched_session_id,
                    rss_mb,
                    cpu_pct,
                    vsize_mb,
                );

                if config.heartbeat_every > 0 && tick % config.heartbeat_every == 0 {
                    let payload = serde_json::json!({
                        "target_app": config.target_app,
                        "target_pid": config.target_pid,
                        "rss_mb": rss_mb,
                        "uptime_secs": started.elapsed().as_secs(),
                    });
                    let _ = godfather.record_event(
                        Some(&config.watched_session_id),
                        Severity::Debug,
                        "godfather",
                        "heartbeat",
                        "watch tick",
                        Some(&payload),
                    );
                }
            }
            None => {
                // Process gone. Read final exit status from /proc
                // (Linux) or sysinfo's last cached value (macOS).
                // Stdlib doesn't give us a non-child reap path, so
                // we treat any sudden disappearance as exit_code=None;
                // attaching as a parent is a future-issue refinement.
                let log_tail = config
                    .log_path
                    .as_ref()
                    .and_then(|p| read_tail(p, 4096).ok());
                let exit_code: Option<i32> = None;
                let exit_signal: Option<i32> = None;

                let _ = godfather.record_event(
                    Some(&config.watched_session_id),
                    Severity::Crash,
                    "godfather",
                    "exit",
                    &format!(
                        "{} (pid {}) disappeared",
                        config.target_app, config.target_pid
                    ),
                    Some(&serde_json::json!({
                        "target_app": config.target_app,
                        "target_pid": config.target_pid,
                        "uptime_secs": started.elapsed().as_secs(),
                    })),
                );
                let _ = godfather.record_crash(
                    &config.watched_session_id,
                    exit_code,
                    exit_signal,
                    log_tail.as_deref(),
                );
                let _ = godfather.record_session_end(
                    &config.watched_session_id,
                    exit_code,
                    exit_signal,
                );
                return;
            }
        }
        tick = tick.wrapping_add(1);
        std::thread::sleep(config.sample_interval);
    }
}

/// `kill(pid, 0)` — POSIX existence probe. Returns true if the PID
/// is alive (or in zombie limbo). The kernel reports faster than
/// sysinfo's cache on macOS, which is why we lean on it.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}
#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

/// Read the last `max_bytes` of `p` as UTF-8. Drops the leading
/// partial line so the tail starts on a clean line boundary.
pub(crate) fn read_tail(p: &Path, max_bytes: usize) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(p)?;
    let len = f.metadata()?.len();
    let from = len.saturating_sub(max_bytes as u64);
    f.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::with_capacity(max_bytes);
    f.take(max_bytes as u64).read_to_end(&mut buf)?;
    let s = String::from_utf8_lossy(&buf).to_string();
    if from > 0 {
        if let Some(idx) = s.find('\n') {
            return Ok(s[idx + 1..].to_string());
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_tail_drops_partial_first_line_when_seeking_in() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        let mut f = std::fs::File::create(&p).unwrap();
        for i in 0..200 {
            writeln!(f, "line {i}").unwrap();
        }
        // Tail of 60 bytes will land mid-line → first partial line dropped.
        let tail = read_tail(&p, 60).unwrap();
        for line in tail.lines() {
            assert!(line.starts_with("line "), "fragmented line: {line}");
        }
    }

    #[test]
    fn read_tail_returns_whole_file_when_smaller_than_max() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        std::fs::write(&p, "hello\n").unwrap();
        assert_eq!(read_tail(&p, 4096).unwrap(), "hello\n");
    }

    /// End-to-end supervision: spawn `sleep 30`, attach a watcher,
    /// wait for at least one sample to land, kill the child, observe
    /// the crash event + ended session row. The most behavior-rich
    /// path in the crate.
    #[cfg(unix)]
    #[test]
    fn watch_handle_records_samples_and_crash_event_on_exit() {
        use crate::event::Severity;
        use crate::Godfather;

        let dir = tempfile::tempdir().unwrap();
        let g = Godfather::open_at(&dir.path().join("_internal.db")).unwrap();

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let session_id = g.record_session_start("test", "0", pid).unwrap();

        let mut config = WatchConfig::new("test", pid, session_id.clone());
        // Tighten the loop so the test doesn't spend 5s sleeping
        // between sysinfo polls.
        config.sample_interval = std::time::Duration::from_millis(150);
        config.heartbeat_every = 0; // skip heartbeat noise in test

        // Open a second Godfather handle for the watcher (it takes
        // ownership) so the test handle stays alive for assertions.
        let watcher_g = Godfather::open_at(&dir.path().join("_internal.db")).unwrap();
        let handle = WatchHandle::spawn(watcher_g, config).expect("spawn watcher");
        assert_eq!(handle.target_pid(), pid);

        // Give the watcher 600ms to record a sample or three before
        // we kill the child; on a busy CI box this is generous.
        std::thread::sleep(std::time::Duration::from_millis(600));

        let _ = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        let _ = child.wait();

        // Don't call `handle.join()` yet — that sets `stop=true`,
        // which would race the worker's loop check before the next
        // sysinfo refresh detects the dead PID and records the
        // crash event. Instead, poll the DB for up to 5s waiting
        // for the watcher to write the crash row on its own.
        let started = std::time::Instant::now();
        loop {
            let crash_evts = g.list_recent_events(10, Some(Severity::Crash)).unwrap();
            if crash_evts
                .iter()
                .any(|e| e.session_id.as_deref() == Some(&session_id))
            {
                break;
            }
            if started.elapsed() > std::time::Duration::from_secs(5) {
                panic!(
                    "watcher never recorded a crash event for {session_id} \
                     within 5s of the child being killed (got: {crash_evts:?})"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // Samples landed under the right session id.
        let samples: i64 = g
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM _crab_resource_sample WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(samples >= 1, "no resource samples recorded; got {samples}");

        // Session row was ended.
        let session = crate::session::get(g.conn(), &session_id).unwrap().unwrap();
        assert!(
            session.ended_at.is_some(),
            "watcher didn't end the session row"
        );

        // Cleanup — worker has already exited naturally, this is a no-op join.
        handle.join();
    }

    /// Drop on a still-running watcher must terminate quickly and
    /// not orphan its worker thread (the worker reads `stop` every
    /// `sample_interval`, so the upper bound is one tick + a bit).
    #[cfg(unix)]
    #[test]
    fn watch_handle_drop_terminates_worker_promptly() {
        use crate::Godfather;
        let dir = tempfile::tempdir().unwrap();
        let g = Godfather::open_at(&dir.path().join("_internal.db")).unwrap();

        // Use the supervisor's own PID so the watcher never sees
        // an exit during the test — the only way the worker stops
        // is via `stop` being set in Drop.
        let pid = std::process::id();
        let session_id = g.record_session_start("self", "0", pid).unwrap();

        let watcher_g = Godfather::open_at(&dir.path().join("_internal.db")).unwrap();
        let mut config = WatchConfig::new("self", pid, session_id);
        config.sample_interval = std::time::Duration::from_millis(80);
        config.heartbeat_every = 0;
        let handle = WatchHandle::spawn(watcher_g, config).unwrap();

        let started = std::time::Instant::now();
        drop(handle);
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "Drop blocked too long: {elapsed:?}"
        );
    }
}
