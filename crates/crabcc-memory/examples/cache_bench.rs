//! In-process micro-bench for the moka caches added in issue #30.
//!
//! Emits a single JSON document on stdout with three sections:
//!
//!   * `palace_registry` — cold vs warm `open_for(cwd)` wall-time
//!   * `git_root_memo`   — raw `find_git_root` vs memoized
//!     `resolve_git_root` over a hot loop
//!   * `embedding_cache` — `HashEmbedder` vs `CachedEmbedder` over a
//!     repeated-text workload
//!
//! Runs in-process so we measure the cache code path directly, not
//! subprocess-spawn overhead. Driven by `bench/cache-bench.py` which
//! reads the JSON, formats a Markdown report, and copies it to
//! `bench/results/cache-bench-<timestamp>.json` + appends to
//! `bench/results/REPORT.md`.

use anyhow::Result;
use crabcc_memory::{
    find_git_root, CachedEmbedder, Embedder, HashEmbedder, PalaceRegistry,
};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

const PALACE_WARM_ITERS: usize = 1_000;
const GIT_ROOT_ITERS: usize = 10_000;
const EMBED_ITERS: usize = 5_000;
const EMBED_DISTINCT_BODIES: usize = 16;

fn time_it<F: FnMut()>(iters: usize, mut f: F) -> Duration {
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    start.elapsed()
}

fn ns_per(d: Duration, iters: usize) -> u64 {
    (d.as_nanos() / iters as u128) as u64
}

fn bench_palace_registry() -> Result<serde_json::Value> {
    let dir = tempdir()?;
    std::fs::create_dir_all(dir.path().join(".git"))?;
    let reg = PalaceRegistry::new();

    // Cold: first open_for must actually create the SQLite file +
    // walk the dir tree. Single-shot to avoid amortizing into noise.
    let cold = {
        let t0 = Instant::now();
        let _ = reg.open_for(dir.path())?;
        t0.elapsed()
    };

    // Warm: the next N calls should be pure cache hits. We `cwd`-pun
    // through both the literal repo root and a nested path so the
    // git-root memo also gets exercised.
    let nested = dir.path().join("a/b/c");
    std::fs::create_dir_all(&nested)?;
    let _ = reg.open_for(&nested)?; // prime memo
    let warm_total = time_it(PALACE_WARM_ITERS, || {
        let _ = reg.open_for(&nested).unwrap();
    });

    let warm_avg_ns = ns_per(warm_total, PALACE_WARM_ITERS);
    let speedup = if warm_avg_ns > 0 {
        cold.as_nanos() as f64 / warm_avg_ns as f64
    } else {
        f64::INFINITY
    };

    Ok(json!({
        "cold_open_ns": cold.as_nanos() as u64,
        "warm_open_ns_avg": warm_avg_ns,
        "warm_iters": PALACE_WARM_ITERS,
        "speedup_x": format!("{speedup:.1}"),
    }))
}

fn bench_git_root_memo() -> Result<serde_json::Value> {
    let dir = tempdir()?;
    std::fs::create_dir_all(dir.path().join(".git"))?;
    let nested = dir.path().join("level1/level2/level3");
    std::fs::create_dir_all(&nested)?;

    // Raw walk: every iteration re-canonicalizes and walks up.
    let raw_total = time_it(GIT_ROOT_ITERS, || {
        let _ = find_git_root(&nested);
    });

    // Memoized: first call populates, the rest are O(1) cache hits.
    let reg = PalaceRegistry::new();
    let _ = reg.resolve_git_root(&nested); // prime
    let memo_total = time_it(GIT_ROOT_ITERS, || {
        let _ = reg.resolve_git_root(&nested);
    });

    let raw_avg = ns_per(raw_total, GIT_ROOT_ITERS);
    let memo_avg = ns_per(memo_total, GIT_ROOT_ITERS);
    let speedup = if memo_avg > 0 {
        raw_avg as f64 / memo_avg as f64
    } else {
        f64::INFINITY
    };

    Ok(json!({
        "iters": GIT_ROOT_ITERS,
        "raw_walk_ns_avg": raw_avg,
        "memoized_ns_avg": memo_avg,
        "speedup_x": format!("{speedup:.1}"),
    }))
}

fn bench_embedding_cache() -> Result<serde_json::Value> {
    // EMBED_DISTINCT_BODIES distinct strings, each embedded N/B times.
    // With CachedEmbedder, only the first B calls hit the inner
    // embedder; the rest are cache hits. With HashEmbedder, every
    // single call recomputes.
    let bodies: Vec<String> = (0..EMBED_DISTINCT_BODIES)
        .map(|i| format!("drawer body sample number {i:04}"))
        .collect();

    // Baseline.
    let raw = HashEmbedder::new();
    let raw_total = time_it(EMBED_ITERS, || {
        let body = &bodies[fastrand_lite() as usize % EMBED_DISTINCT_BODIES];
        let _ = raw.embed_one(body).unwrap();
    });

    // Decorated.
    let cached = CachedEmbedder::new(Arc::new(HashEmbedder::new()));
    // Prime — the first round of EMBED_DISTINCT_BODIES calls fills
    // the cache. After that every body should be a cache hit.
    for b in &bodies {
        let _ = cached.embed_one(b)?;
    }
    let cached_total = time_it(EMBED_ITERS, || {
        let body = &bodies[fastrand_lite() as usize % EMBED_DISTINCT_BODIES];
        let _ = cached.embed_one(body).unwrap();
    });

    let raw_avg = ns_per(raw_total, EMBED_ITERS);
    let cached_avg = ns_per(cached_total, EMBED_ITERS);
    let speedup = if cached_avg > 0 {
        raw_avg as f64 / cached_avg as f64
    } else {
        f64::INFINITY
    };

    Ok(json!({
        "iters": EMBED_ITERS,
        "distinct_bodies": EMBED_DISTINCT_BODIES,
        "raw_embed_ns_avg": raw_avg,
        "cached_embed_ns_avg": cached_avg,
        "speedup_x": format!("{speedup:.1}"),
        "cache_entries": cached.cache_entry_count(),
    }))
}

/// xorshift32 for fast deterministic body picking. No external crate.
fn fastrand_lite() -> u32 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u32> = const { Cell::new(0x12345678) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        s.set(x);
        x
    })
}

fn main() -> Result<()> {
    // Touch a Path import so clippy doesn't gripe in case the module
    // shape changes.
    let _ = Path::new(".");

    let report = json!({
        "schema": "crabcc.cache-bench.v1",
        "palace_registry": bench_palace_registry()?,
        "git_root_memo": bench_git_root_memo()?,
        "embedding_cache": bench_embedding_cache()?,
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
