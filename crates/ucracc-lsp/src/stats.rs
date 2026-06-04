//! Lightweight, local-only usage / error / perf stats for the LSP.
//!
//! Goals: cheap enough to leave always-on (atomic counters, no allocation on
//! the hot path) and useful for future development. Three ways to read it:
//!   1. the `ucracc.stats` executeCommand returns a JSON snapshot live;
//!   2. a snapshot is written to `~/.crabcc/ucracc-lsp-stats.json` on shutdown
//!      (unless `CRABCC_NO_TELEMETRY=1`);
//!   3. every recorded request emits a structured `tracing` event on target
//!      `ucracc_lsp::stats` (method, microseconds, error) so an external OTLP
//!      or JSON-log collector can scrape it without touching this process.
//!
//! Local-only: nothing is sent over the network from here.

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Instant;

#[derive(Default)]
pub struct MethodStat {
    pub count: AtomicU64,
    pub errors: AtomicU64,
    pub total_us: AtomicU64,
    pub max_us: AtomicU64,
}

pub struct Stats {
    started: Instant,
    methods: DashMap<&'static str, MethodStat>,
}

impl Stats {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            methods: DashMap::new(),
        }
    }

    /// Record one request: bump count, errors, total + max latency, and emit a
    /// structured tracing event. Never blocks beyond a per-method shard lock.
    pub fn record(&self, method: &'static str, elapsed_us: u64, is_err: bool) {
        let e = self.methods.entry(method).or_default();
        e.count.fetch_add(1, Relaxed);
        if is_err {
            e.errors.fetch_add(1, Relaxed);
        }
        e.total_us.fetch_add(elapsed_us, Relaxed);
        let mut cur = e.max_us.load(Relaxed);
        while elapsed_us > cur {
            match e
                .max_us
                .compare_exchange_weak(cur, elapsed_us, Relaxed, Relaxed)
            {
                Ok(_) => break,
                Err(observed) => cur = observed,
            }
        }
        tracing::info!(
            target: "ucracc_lsp::stats",
            method,
            us = elapsed_us,
            err = is_err,
            "lsp_request"
        );
    }

    /// JSON snapshot: per-method count/errors/avg_ms/max_ms + totals + uptime.
    pub fn snapshot(&self) -> serde_json::Value {
        let mut per = serde_json::Map::new();
        let (mut total, mut total_err) = (0u64, 0u64);
        for kv in self.methods.iter() {
            let m = kv.value();
            let count = m.count.load(Relaxed);
            let errors = m.errors.load(Relaxed);
            let total_us = m.total_us.load(Relaxed);
            let max_us = m.max_us.load(Relaxed);
            total += count;
            total_err += errors;
            per.insert(
                kv.key().to_string(),
                serde_json::json!({
                    "count": count,
                    "errors": errors,
                    "avg_ms": if count > 0 { (total_us as f64 / count as f64) / 1000.0 } else { 0.0 },
                    "max_ms": max_us as f64 / 1000.0,
                }),
            );
        }
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "uptime_secs": self.started.elapsed().as_secs(),
            "total_requests": total,
            "total_errors": total_err,
            "methods": per,
        })
    }

    /// Best-effort write of a snapshot to `~/.crabcc/ucracc-lsp-stats.json`.
    /// No-op (returns Ok) if `CRABCC_NO_TELEMETRY=1` or the home dir is unknown.
    pub fn dump_to_home(&self) {
        if std::env::var("CRABCC_NO_TELEMETRY").as_deref() == Ok("1") {
            return;
        }
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            return;
        };
        let dir = home.join(".crabcc");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let path = dir.join("ucracc-lsp-stats.json");
        if let Ok(s) = serde_json::to_string_pretty(&self.snapshot()) {
            let _ = std::fs::write(path, s);
        }
    }
}

impl Default for Stats {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII timer: records `method` duration on drop. Default outcome is success;
/// call [`Timer::fail`] to mark an error before it drops.
pub struct Timer {
    stats: Arc<Stats>,
    method: &'static str,
    start: Instant,
    err: bool,
}

impl Timer {
    pub fn start(stats: Arc<Stats>, method: &'static str) -> Self {
        Self {
            stats,
            method,
            start: Instant::now(),
            err: false,
        }
    }

    pub fn fail(&mut self) {
        self.err = true;
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let us = self.start.elapsed().as_micros() as u64;
        self.stats.record(self.method, us, self.err);
    }
}
