//! Microbench for the godfather hot inserts (#488).
//!
//! Two groups:
//!
//!   * `record_resource_sample` — the watcher's per-tick path.
//!     Multiplied by N concurrent supervisors at 5 s intervals,
//!     so even a 1 µs reduction is worth catching.
//!   * `list_recent_events` — the dashboard's poll-shaped read.
//!     Hits 1 000 rows; mostly a SQLite-decode + `Severity::parse_str`
//!     loop.
//!
//! Run via `cargo bench -p crabcc-godfather`. Each bench writes an
//! HTML report to `target/criterion/` for visual diff.

use std::hint::black_box;
use std::time::Duration;

use crabcc_godfather::event::Severity;
use crabcc_godfather::godfather::ResourceSample;
use crabcc_godfather::{Godfather, InstallSource};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

fn fresh_godfather() -> (Godfather, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("_internal.db");
    let g = Godfather::open_at(&path).unwrap();
    g.record_install_once("bench", InstallSource::Source)
        .unwrap();
    g.record_host_info().unwrap();
    (g, dir)
}

fn bench_resource_sample(c: &mut Criterion) {
    let mut group = c.benchmark_group("record_resource_sample");
    // 5 s sample interval × ~3 supervisors = ~36 inserts/min in
    // production, so picking 1 000 here is generous on the
    // hot-path side and small enough to run in a few hundred ms.
    group.throughput(criterion::Throughput::Elements(1));
    group.measurement_time(Duration::from_secs(3));

    group.bench_function("single_insert", |b| {
        let (g, _dir) = fresh_godfather();
        let sid = g.record_session_start("bench", "0", 1234).unwrap();
        b.iter(|| {
            g.record_resource_sample(black_box(&sid), 256, 12.5, 1024)
                .unwrap();
        });
    });

    group.bench_function("1000_inserts", |b| {
        b.iter_batched(
            fresh_godfather,
            |(g, _dir)| {
                let sid = g.record_session_start("bench", "0", 1234).unwrap();
                for _ in 0..1_000 {
                    g.record_resource_sample(&sid, 256, 12.5, 1024).unwrap();
                }
            },
            BatchSize::SmallInput,
        );
    });

    // The batched API the WatchHandle uses (#488). Same 1 000-insert
    // workload, one transaction. Expected: most of the time vanishes
    // because the per-insert COMMIT / fsync is amortised across the
    // whole batch.
    group.bench_function("1000_inserts_batched", |b| {
        b.iter_batched(
            || {
                let (g, dir) = fresh_godfather();
                let sid = g.record_session_start("bench", "0", 1234).unwrap();
                let samples: Vec<ResourceSample> = (0..1_000)
                    .map(|_| ResourceSample {
                        ts: 1_700_000_000,
                        rss_mb: 256,
                        cpu_pct: 12.5,
                        vsize_mb: 1024,
                    })
                    .collect();
                (g, dir, sid, samples)
            },
            |(g, _dir, sid, samples)| {
                g.record_resource_samples(&sid, &samples).unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn bench_list_recent_events(c: &mut Criterion) {
    let mut group = c.benchmark_group("list_recent_events");
    group.measurement_time(Duration::from_secs(3));

    // One-time fill: 1 000 events of mixed severity.
    let (g, _dir) = fresh_godfather();
    let sid = g.record_session_start("bench", "0", 1234).unwrap();
    for i in 0..1_000 {
        let sev = match i % 5 {
            0 => Severity::Debug,
            1 => Severity::Info,
            2 => Severity::Warn,
            3 => Severity::Error,
            _ => Severity::Crash,
        };
        g.record_event(Some(&sid), sev, "bench", "tick", "test", None)
            .unwrap();
    }

    group.bench_function("100_unfiltered", |b| {
        b.iter(|| {
            let v = g.list_recent_events(100, None).unwrap();
            black_box(v);
        });
    });

    group.bench_function("100_warn_plus", |b| {
        b.iter(|| {
            let v = g.list_recent_events(100, Some(Severity::Warn)).unwrap();
            black_box(v);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_resource_sample, bench_list_recent_events);
criterion_main!(benches);
