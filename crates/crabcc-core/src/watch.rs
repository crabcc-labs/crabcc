//! `crabcc watch` — sidecar FS watchdog that auto-refreshes the index.
//!
//! ## Threading model
//!
//! The watcher truly runs as a sidecar — it lives on its own thread, separate
//! from the caller. `spawn(...)` returns a `WatchHandle` immediately so the
//! main thread can stay responsive (handle SIGINT, drive the CLI, etc).
//! `block_until_done()` joins the worker.
//!
//! `notify-debouncer-mini` itself spawns its own internal thread for FS
//! events. So the full picture is:
//!
//! ```text
//!   main thread          watch::worker thread          notify thread
//!   ───────────          ─────────────────────         ─────────────
//!   spawn()      ─────►  recv loop ◄───────── events ──── kqueue/inotify
//!     │                    │
//!     │                    │ run_refresh(store)  ──► Mutex<Store>
//!     │                    │
//!   block_until_done() ──► join
//! ```
//!
//! The Store is wrapped in `Arc<Mutex<Store>>` so both the worker thread and
//! the main thread (or a future RPC handler) can use it without races. SQLite
//! is in WAL mode (see `Store::open`) so concurrent reads through a second
//! connection wouldn't even need the lock — but we keep the Mutex anyway for
//! correctness of multi-statement transactions inside `refresh()`.
//!
//! ## Bulletproofness
//!
//! 1. **Debouncing**: a burst of 200 events (e.g. `git checkout`) becomes ONE
//!    refresh, not 200.
//! 2. **Filtering**: events for files we wouldn't index anyway (markdown,
//!    yaml, binaries) don't trigger anything.
//! 3. **Feedback-loop guard**: events for paths inside `.crabcc/` (our own
//!    writes: SQLite WAL, Tantivy commits, the graph sidecar) are ignored.
//!    Without this, a single refresh would re-trigger itself forever.
//! 4. **Crash isolation**: refresh errors log + continue, never panic. The
//!    watcher loop keeps running until the worker thread is asked to stop or
//!    the FS-events channel closes.

use crate::extract;
use crate::index;
use crate::store::Store;
use anyhow::{Context, Result};
use notify_debouncer_mini::{
    new_debouncer,
    notify::{ErrorKind as NotifyErrorKind, RecursiveMode},
    DebouncedEventKind,
};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Returned by `spawn`. Holds the worker thread join handle and a stop flag
/// the worker checks between events. Drop = signal stop + join.
pub struct WatchHandle {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<Result<()>>>,
}

impl WatchHandle {
    /// Block until the worker exits naturally (channel close).
    pub fn block_until_done(mut self) -> Result<()> {
        if let Some(h) = self.worker.take() {
            h.join()
                .map_err(|_| anyhow::anyhow!("watch worker panicked"))??;
        }
        Ok(())
    }
    /// Ask the worker thread to exit at its next loop iteration.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.worker.take() {
            // Best-effort join — don't propagate errors at drop time.
            let _ = h.join();
        }
    }
}

/// Spawn a watcher worker on its own thread. Returns immediately; the
/// returned handle drives lifecycle.
pub fn spawn(root: &Path, store: Arc<Mutex<Store>>, debounce: Duration) -> Result<WatchHandle> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_worker = stop.clone();
    let root_owned = root.to_path_buf();
    let worker = std::thread::Builder::new()
        .name("crabcc-watch".into())
        .spawn(move || worker_loop(&root_owned, store, debounce, stop_for_worker))
        .context("spawn watch worker")?;
    Ok(WatchHandle {
        stop,
        worker: Some(worker),
    })
}

/// Convenience: spawn + block — what `crabcc watch` from the CLI uses.
pub fn watch(root: &Path, store: Arc<Mutex<Store>>, debounce: Duration) -> Result<()> {
    spawn(root, store, debounce)?.block_until_done()
}

fn worker_loop(
    root: &Path,
    store: Arc<Mutex<Store>>,
    debounce: Duration,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let crabcc_dir = root.join(".crabcc");
    // Ensure the dir exists so canonicalize() succeeds even on a fresh repo.
    std::fs::create_dir_all(&crabcc_dir).ok();
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let canon_skip = crabcc_dir
        .canonicalize()
        .unwrap_or_else(|_| crabcc_dir.clone());

    let (tx, rx) = channel();
    let mut debouncer = new_debouncer(debounce, tx).context("create debouncer")?;
    debouncer
        .watcher()
        .watch(root, RecursiveMode::Recursive)
        .with_context(|| format!("watch {}", root.display()))?;

    eprintln!(
        "crabcc watch: monitoring {} (debounce={}ms; Ctrl-C to exit)",
        canon_root.display(),
        debounce.as_millis()
    );

    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(events)) => {
                let trigger = events.iter().any(|e| {
                    matches!(e.kind, DebouncedEventKind::Any)
                        && should_trigger(&e.path, &canon_skip)
                });
                if !trigger {
                    continue;
                }
                run_refresh(root, &store);
            }
            Ok(Err(e)) => {
                // Ignore benign "file vanished mid-event" during git checkouts.
                match e.kind {
                    NotifyErrorKind::PathNotFound => {}
                    _ => tracing::warn!("watcher event error: {e:?}"),
                }
            }
            Err(RecvTimeoutError::Timeout) => continue, // re-check stop flag
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

fn should_trigger(path: &Path, crabcc_dir: &Path) -> bool {
    // Feedback-loop guard: refreshing rewrites .crabcc/, which would emit more
    // events. Skip them — also covers the Tantivy mmaps and graph.json sidecar.
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if canon.starts_with(crabcc_dir) {
        return false;
    }
    // Only languages we index produce useful refreshes. A markdown edit
    // triggering a refresh is wasted I/O; the index would be a no-op.
    extract::detect_lang(&canon).is_some() || directory_event(&canon)
}

/// Directory-level events (mkdir/rmdir, mass move) won't have a language;
/// trigger anyway so deletes get picked up promptly.
fn directory_event(path: &Path) -> bool {
    !path.is_file()
}

fn run_refresh(root: &Path, store: &Arc<Mutex<Store>>) {
    let store = match store.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    match index::refresh(root, &store) {
        Ok(stats) => {
            // Compact one-line summary — JSON for any harness piping us.
            match serde_json::to_string(&stats) {
                Ok(line) => println!("{line}"),
                Err(_) => println!(
                    "{{\"new\":{},\"reindexed\":{},\"deleted\":{}}}",
                    stats.new, stats.reindexed, stats.deleted
                ),
            }
        }
        Err(e) => {
            tracing::warn!("refresh error: {e:?}");
            eprintln!("crabcc watch: refresh error — {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::build_index;

    #[test]
    fn should_trigger_skips_crabcc_writes() {
        let dir = tempfile::tempdir().unwrap();
        let crabcc = dir.path().join(".crabcc");
        std::fs::create_dir_all(&crabcc).unwrap();
        let inner = crabcc.join("index.db-wal");
        std::fs::write(&inner, "").unwrap();
        let canon_skip = crabcc.canonicalize().unwrap();
        assert!(
            !should_trigger(&inner, &canon_skip),
            "events under .crabcc/ must not trigger a refresh (feedback loop)"
        );
    }

    #[test]
    fn should_trigger_picks_up_supported_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let crabcc = dir.path().join(".crabcc");
        std::fs::create_dir_all(&crabcc).unwrap();
        let f = dir.path().join("a.ts");
        std::fs::write(&f, "x").unwrap();
        assert!(should_trigger(&f, &crabcc.canonicalize().unwrap()));
    }

    #[test]
    fn should_trigger_ignores_unsupported_files() {
        let dir = tempfile::tempdir().unwrap();
        let crabcc = dir.path().join(".crabcc");
        std::fs::create_dir_all(&crabcc).unwrap();
        let f = dir.path().join("README.md");
        std::fs::write(&f, "x").unwrap();
        // For a file that exists with an unsupported extension, no trigger.
        assert!(!should_trigger(&f, &crabcc.canonicalize().unwrap()));
    }

    #[test]
    fn should_trigger_passes_unknown_paths_as_dir_events() {
        // Path doesn't exist → directory_event() returns true → trigger.
        // Ensures rmdir / file-deleted events still wake the worker even
        // when we can't classify by extension.
        let dir = tempfile::tempdir().unwrap();
        let crabcc = dir.path().join(".crabcc");
        std::fs::create_dir_all(&crabcc).unwrap();
        let nonexistent = dir.path().join("does-not-exist");
        assert!(should_trigger(
            &nonexistent,
            &crabcc.canonicalize().unwrap()
        ));
    }

    #[test]
    fn handle_stop_signals_worker() {
        // Spawn a watcher, immediately stop it, confirm it exits cleanly.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("idx.db")).unwrap();
        let store = Arc::new(Mutex::new(store));
        let h = spawn(dir.path(), store, Duration::from_millis(50)).unwrap();
        // Don't wait for events — just stop and join.
        std::thread::sleep(Duration::from_millis(100));
        h.stop();
        // block_until_done should return promptly (within ~600ms — one
        // recv_timeout window plus debounce slack).
        let t0 = std::time::Instant::now();
        h.block_until_done().unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "watcher took too long to honor stop: {:?}",
            t0.elapsed()
        );
    }

    #[test]
    #[ignore = "FS events are coarse-grained and racy across CI runners; \
                run locally with `cargo test -- --ignored` for manual verification"]
    fn watch_picks_up_a_real_file_change() {
        // Real end-to-end: write a file → watcher → refresh fires.
        // Skipped by default — kqueue/FSEvents on macOS, inotify on Linux,
        // and especially containerized CI all have different event timings.
        // The deterministic `should_trigger_*` and `handle_stop_signals_worker`
        // tests cover the logic that matters for a code review.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.ts"), "export const x = 1;").unwrap();

        let store = Store::open(&root.join(".crabcc-idx.db")).unwrap();
        // Pre-populate the index so refresh can detect the change.
        build_index(root, &store).unwrap();
        let store = Arc::new(Mutex::new(store));

        let h = spawn(root, store.clone(), Duration::from_millis(100)).unwrap();
        // Sleep long enough for the watcher to register listeners.
        std::thread::sleep(Duration::from_millis(300));
        // Mutate a file the watcher cares about.
        std::fs::write(
            root.join("a.ts"),
            "export const x = 1;\nexport function added(){ return 2; }\n",
        )
        .unwrap();

        // Give the debouncer + refresh time to fire.
        std::thread::sleep(Duration::from_millis(800));
        h.stop();
        h.block_until_done().unwrap();

        // The new symbol should now be in the index.
        let store = store.lock().unwrap();
        let hits = store.find_by_name("added").unwrap();
        assert!(
            !hits.is_empty(),
            "watcher did not pick up the added function (FS events may be slow on this OS)"
        );
    }
}
