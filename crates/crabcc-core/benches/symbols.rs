// Criterion micro-benches for crabcc-core hot paths on the SQLite symbol store.
//
// Wired by Agent A in `crates/crabcc-core/Cargo.toml`:
//   [dev-dependencies]
//   criterion = "0.5"
//   tempfile  = { workspace = true }
//
//   [[bench]]
//   name    = "symbols"
//   harness = false
//
// Run with `cargo bench -p crabcc-core`.
//
// What's measured:
//   * `find_by_name_cold`     — store opened-then-reopened, page cache cold-ish,
//                                exercises the `idx_symbols_name` lookup path.
//   * `find_by_name_warm`     — same row, hot connection, hot page cache.
//   * `iter_all_symbols`      — full-table scan + materialization to Vec<Symbol>.
//   * `replace_symbols_1k`    — bulk insert path used by the indexer.
//
// All fixtures are deterministic so wall-time differences across runs come
// from the code, not the data.

use crabcc_core::store::Store;
use crabcc_core::types::{Symbol, SymbolKind};
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use std::path::PathBuf;
use tempfile::TempDir;

const N_SYMBOLS: usize = 10_000;
const TARGET_NAME: &str = "sym_05000";

/// Deterministic synthetic symbol generator. Keeps name distribution dense
/// and predictable so `find_by_name(TARGET_NAME)` always hits a real row.
fn synth_symbols(n: usize) -> Vec<Symbol> {
    (0..n)
        .map(|i| Symbol {
            name: format!("sym_{i:05}"),
            kind: if i % 2 == 0 {
                SymbolKind::Function
            } else {
                SymbolKind::Method
            },
            signature: Some(format!("fn sym_{i:05}(x: i32) -> i32")),
            // Cheap deterministic "parent" to exercise the nullable column.
            parent: if i % 3 == 0 {
                None
            } else {
                Some(format!("Mod{}", i % 17))
            },
            file: format!("synthetic/path_{}.rs", i % 64),
            line_start: (i as u32) * 4 + 1,
            line_end: (i as u32) * 4 + 3,
            visibility: (i % 5 == 0).then_some("pub".into()),
        })
        .collect()
}

/// Build a fresh store at `tmp/index.db` populated with `N_SYMBOLS` symbols
/// across 64 synthetic files. Returns the TempDir (must outlive the Store)
/// and the populated store.
fn make_populated_store(n: usize) -> (TempDir, Store, PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("index.db");
    let store = Store::open(&db_path).expect("open store");

    // 64 synthetic files; symbols evenly distributed across them.
    let symbols = synth_symbols(n);
    let per_file: std::collections::HashMap<usize, Vec<Symbol>> = {
        let mut m: std::collections::HashMap<usize, Vec<Symbol>> = std::collections::HashMap::new();
        for s in symbols {
            let bucket = s
                .file
                .rsplit('_')
                .next()
                .and_then(|t| t.trim_end_matches(".rs").parse::<usize>().ok())
                .unwrap_or(0);
            m.entry(bucket).or_default().push(s);
        }
        m
    };
    for (bucket, syms) in per_file {
        let path = format!("synthetic/path_{bucket}.rs");
        let file_id = store
            .upsert_file(&path, "deadbeef", 0, "rust")
            .expect("upsert_file");
        store
            .replace_symbols(file_id, &syms)
            .expect("replace_symbols");
    }
    (tmp, store, db_path)
}

fn bench_find_by_name_cold(c: &mut Criterion) {
    let (tmp, _store, db_path) = make_populated_store(N_SYMBOLS);
    // Drop the populating store so the bench reopens a fresh connection
    // each iteration — measures "open + first query" cost (cold-ish; the
    // OS page cache is still warm, but the rusqlite prepared-statement
    // cache and SQLite per-conn cache are empty).
    drop(_store);

    let mut group = c.benchmark_group("find_by_name_cold");
    group.sample_size(20); // each iter does open + query, so keep small
    group.bench_function("reopen_then_find_one", |b| {
        b.iter(|| {
            let s = Store::open(&db_path).expect("reopen");
            let hits = s.find_by_name(TARGET_NAME).expect("find");
            assert!(!hits.is_empty());
            criterion::black_box(hits);
        })
    });
    group.finish();
    drop(tmp);
}

fn bench_find_by_name_warm(c: &mut Criterion) {
    let (tmp, store, _db_path) = make_populated_store(N_SYMBOLS);

    let mut group = c.benchmark_group("find_by_name_warm");
    // 100 lookups per iteration to amortize criterion's measurement loop
    // overhead — the per-call cost is dominated by SQLite, not framework.
    group.throughput(Throughput::Elements(100));
    group.bench_function("hot_lookup_x100", |b| {
        b.iter(|| {
            for _ in 0..100 {
                let hits = store.find_by_name(TARGET_NAME).expect("find");
                criterion::black_box(hits);
            }
        })
    });
    group.finish();
    drop(tmp);
}

fn bench_iter_all_symbols(c: &mut Criterion) {
    let (tmp, store, _db_path) = make_populated_store(N_SYMBOLS);

    let mut group = c.benchmark_group("iter_all_symbols");
    group.throughput(Throughput::Elements(N_SYMBOLS as u64));
    group.sample_size(20);
    group.bench_function("full_scan_10k", |b| {
        b.iter(|| {
            let all = store.iter_all_symbols().expect("iter");
            assert_eq!(all.len(), N_SYMBOLS);
            criterion::black_box(all);
        })
    });
    group.finish();
    drop(tmp);
}

fn bench_replace_symbols_1k(c: &mut Criterion) {
    // Fresh store per bench; we don't want the symbols table growing
    // across iterations (that would skew later iterations).
    let mut group = c.benchmark_group("replace_symbols_1k");
    group.throughput(Throughput::Elements(1_000));
    group.sample_size(20);
    group.bench_function("insert_1000", |b| {
        let symbols = synth_symbols(1_000);
        b.iter_with_setup(
            || {
                let tmp = TempDir::new().expect("tempdir");
                let db_path = tmp.path().join("index.db");
                let store = Store::open(&db_path).expect("open store");
                let file_id = store
                    .upsert_file("synthetic/insert_target.rs", "cafebabe", 0, "rust")
                    .expect("upsert_file");
                (tmp, store, file_id)
            },
            |(_tmp, store, file_id)| {
                store
                    .replace_symbols(file_id, &symbols)
                    .expect("replace_symbols");
                criterion::black_box(&store);
            },
        )
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_find_by_name_cold,
    bench_find_by_name_warm,
    bench_iter_all_symbols,
    bench_replace_symbols_1k,
);
criterion_main!(benches);
