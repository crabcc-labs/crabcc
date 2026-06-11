// SQLite DB-layer load micro-benchmarks for crabcc-core.
//
// Measures raw database throughput independent of the tree-sitter extraction
// pipeline — just the storage engine under bulk write and read pressure.
//
// Run:
//   cargo bench -p crabcc-core --features bench --bench db_load
//   cargo bench -p crabcc-core --features bench --bench db_load -- --warm-up-time 1
//
// What's measured:
//   * bulk_insert_symbols_{1k,10k,50k} — full replace_symbols path per file
//   * bulk_insert_edges_{1k,10k}        — edge creation between synthetic symbols
//   * cold_name_lookup                   — find_by_name on a freshly-opened store
//   * warm_name_lookup                   — same query, hot page cache
//   * pragma_defaults_vs_tuned           — same 10k insert with stock vs tuned PRAGMAs

use crabcc_core::store::Store;
use crabcc_core::types::{Symbol, SymbolKind};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;
use tempfile::TempDir;

// ── fixtures ──────────────────────────────────────────────────────────────

/// Deterministic synthetic symbols. Name = `sym_{i:05}`, spread across
/// `n_files` file paths in round-robin order so each file gets ~n/n_files rows.
fn synth_symbols(n: usize, n_files: usize) -> Vec<Symbol> {
    (0..n)
        .map(|i| Symbol {
            name: format!("sym_{i:05}"),
            kind: match i % 7 {
                0 => SymbolKind::Function,
                1 => SymbolKind::Method,
                2 => SymbolKind::Struct,
                3 => SymbolKind::Enum,
                4 => SymbolKind::Trait,
                5 => SymbolKind::Const,
                _ => SymbolKind::Var,
            },
            signature: Some(format!("fn sym_{i:05}(x: i32) -> i32")),
            parent: (i % 5 != 0).then(|| format!("Mod{}", i % 23)),
            file: format!("src/module_{}.rs", i % n_files),
            line_start: (i as u32) * 4 + 1,
            line_end: (i as u32) * 4 + 3,
            visibility: (i % 7 == 0).then_some("pub".into()),
        })
        .collect()
}

/// Group symbols by their `.file` field, returning `(file_path, Vec<Symbol>)`.
fn group_by_file(symbols: &[Symbol]) -> Vec<(String, Vec<Symbol>)> {
    let mut map: std::collections::HashMap<String, Vec<Symbol>> = std::collections::HashMap::new();
    for s in symbols {
        map.entry(s.file.clone()).or_default().push(s.clone());
    }
    // Deterministic order so benchmarks are reproducible.
    let mut entries: Vec<_> = map.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

/// Populate a store with symbols spread across `n_files` files. Uses
/// `write_batch` (synchronous=OFF) for bulk speed. Returns the tempdir
/// (must outlive the store) and the store.
fn populate_store(n_symbols: usize, n_files: usize) -> (TempDir, Store) {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("index.db");
    let store = Store::open(&db_path).expect("open store");

    let symbols = synth_symbols(n_symbols, n_files);
    let by_file = group_by_file(&symbols);

    store
        .write_batch(|s| {
            for (path, syms) in &by_file {
                let fid = s
                    .upsert_file(path, "aabbccddeeff00112233445566778899", 0, "rust")
                    .expect("upsert_file");
                s.replace_symbols(fid, syms).expect("replace_symbols");
            }
            Ok(())
        })
        .expect("write_batch");

    (tmp, store)
}

/// Create a small store for edge benchmarks: N symbols in one file, then
/// create call edges between adjacent pairs (0→1, 1→2, … N-2→N-1).
fn populate_with_edges(n_symbols: usize) -> (TempDir, Store, Vec<i64>) {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("index.db");
    let store = Store::open(&db_path).expect("open store");

    let symbols = synth_symbols(n_symbols, 1);
    let fid = store
        .upsert_file("src/main.rs", "aabbccddeeff00112233445566778899", 0, "rust")
        .expect("upsert_file");

    store
        .write_batch(|s| {
            s.replace_symbols(fid, &symbols).expect("replace_symbols");
            Ok(())
        })
        .expect("write_batch");

    // Resolve all symbol ids.
    let ids: Vec<i64> = symbols
        .iter()
        .map(|sym| {
            store
                .symbol_id_by_name_file(&sym.name, fid)
                .expect("symbol_id_by_name_file")
                .expect("symbol exists")
        })
        .collect();

    // Create call edges between adjacent pairs.
    store
        .write_batch(|s| {
            for w in ids.windows(2) {
                let (src, dst) = (w[0], w[1]);
                s.conn()
                    .execute(
                        "INSERT OR IGNORE INTO edges(src_symbol_id, dst_symbol_id, kind, line) \
                         VALUES (?1, ?2, 'call', ?3)",
                        rusqlite::params![src, dst, 1],
                    )
                    .expect("insert edge");
            }
            Ok(())
        })
        .expect("write_batch");

    (tmp, store, ids)
}

// ── bench groups ───────────────────────────────────────────────────────────

/// Bulk symbol insert throughput at three scales.
/// Each iteration builds a fresh store from scratch — measures the full
/// `upsert_file` + `replace_symbols` path including transaction overhead.
fn bench_bulk_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_insert");
    for &(n_syms, n_files) in &[(1_000, 32), (10_000, 128), (50_000, 512)] {
        let symbols = synth_symbols(n_syms, n_files);
        let by_file = group_by_file(&symbols);
        let row_count = n_syms as u64;

        group.throughput(Throughput::Elements(row_count));
        group.bench_with_input(
            BenchmarkId::new("symbols", format!("{n_syms}s_{n_files}f")),
            &by_file,
            |b, files| {
                b.iter(|| {
                    let tmp = TempDir::new().expect("tempdir");
                    let db = tmp.path().join("index.db");
                    let store = Store::open(&db).expect("open");
                    store
                        .write_batch(|s| {
                            for (path, syms) in files {
                                let fid = s
                                    .upsert_file(
                                        path,
                                        "aabbccddeeff00112233445566778899",
                                        0,
                                        "rust",
                                    )
                                    .expect("upsert");
                                s.replace_symbols(fid, syms).expect("replace");
                            }
                            Ok(())
                        })
                        .expect("write_batch");
                    black_box(store);
                });
            },
        );
    }
    group.finish();
}

/// Edge creation throughput. Reuses an already-populated symbol store and
/// measures the cost of inserting N call edges.
fn bench_edge_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("edge_insert");
    for &n in &[1_000, 10_000] {
        let (tmp, store, ids) = populate_with_edges(n);
        // We already inserted edges in populate_with_edges; measure re-insert
        // (INSERT OR IGNORE makes it a no-op, but the B-tree probe still costs).
        // For a real write benchmark, drop edges first.
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(n),
            &(store, ids),
            |b, (store, ids)| {
                b.iter(|| {
                    store
                        .write_batch(|s| {
                            let conn = s.conn();
                            conn.execute("DELETE FROM edges", []).expect("delete edges");
                            for w in ids.windows(2) {
                                conn.execute(
                                    "INSERT INTO edges(src_symbol_id, dst_symbol_id, kind, line) \
                                     VALUES (?1, ?2, 'call', ?3)",
                                    rusqlite::params![w[0], w[1], 1],
                                )
                                .expect("insert edge");
                            }
                            Ok(())
                        })
                        .expect("write_batch");
                    black_box(());
                });
            },
        );
        // Keep tmp alive for the group.
        black_box(&tmp);
    }
    group.finish();
}

/// Cold-cache name lookup: open store, query one symbol, discard.
fn bench_cold_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("lookup_cold");
    for &n in &[1_000, 10_000, 50_000] {
        let (tmp, _store) = populate_store(n, (n / 32).max(1));
        let db_path = tmp.path().join("index.db");
        // Drop the hot connection and reopen so the page cache starts cold.
        drop(_store);

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(n), &db_path, |b, db_path| {
            b.iter(|| {
                let store = Store::open(db_path).expect("open");
                let hits = store.find_by_name("sym_00420").expect("find_by_name");
                black_box(hits);
                // store dropped here → close connection
            });
        });
    }
    group.finish();
}

/// Warm-cache name lookup: open store once, query repeatedly.
fn bench_warm_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("lookup_warm");
    for &n in &[1_000, 10_000, 50_000] {
        let (tmp, store) = populate_store(n, (n / 32).max(1));

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(n), &store, |b, store| {
            b.iter(|| {
                black_box(store.find_by_name("sym_00420").expect("find_by_name"));
            });
        });
        black_box(&tmp);
    }
    group.finish();
}

/// PRAGMA sensitivity: same 10k insert with default SQLite settings vs our
/// tuned production settings. Measures the wall-time delta attributable to
/// page_size, cache_size, mmap, and temp_store.
fn bench_pragma_sensitivity(c: &mut Criterion) {
    let mut group = c.benchmark_group("pragma_sensitivity");
    let symbols = synth_symbols(10_000, 64);
    let by_file = group_by_file(&symbols);

    // "Stock" SQLite: only WAL + NORMAL sync (minimum for correctness).
    // No mmap, no cache bump, no temp_store, default 4 KB pages.
    group.bench_function("stock_defaults", |b| {
        b.iter(|| {
            let tmp = TempDir::new().expect("tempdir");
            let db = tmp.path().join("index.db");
            let conn = rusqlite::Connection::open(&db).expect("open");
            conn.pragma_update(None, "journal_mode", "WAL").ok();
            conn.pragma_update(None, "synchronous", "NORMAL").ok();
            conn.pragma_update(None, "foreign_keys", "ON").ok();
            // Intentionally omit: mmap_size, cache_size, temp_store, page_size.
            conn.execute_batch(include_str!("../../../schema/001_init.sql"))
                .expect("schema");
            let store = Store::open(&db).expect("open"); // re-opens with our PRAGMAs
                                                         // Override back to stock for the bench.
            store
                .conn()
                .pragma_update(None, "cache_size", -2_000_i64)
                .ok(); // 2 MB default
            store.conn().pragma_update(None, "mmap_size", 0_i64).ok();
            store.conn().pragma_update(None, "temp_store", 0_i64).ok(); // DEFAULT

            store
                .write_batch(|s| {
                    for (path, syms) in &by_file {
                        let fid = s
                            .upsert_file(path, "aabbccddeeff00112233445566778899", 0, "rust")
                            .expect("upsert");
                        s.replace_symbols(fid, syms).expect("replace");
                    }
                    Ok(())
                })
                .expect("write_batch");
            black_box(store);
        });
    });

    // "Tuned": our production settings (64 MB cache, 30 GB mmap, MEMORY temp_store).
    group.bench_function("tuned_production", |b| {
        b.iter(|| {
            let (tmp, store) = populate_store(10_000, 64);
            black_box((tmp, store));
        });
    });

    group.finish();
}

/// Full-table scan: `iter_all_symbols` materializes every row + decompresses
/// signatures. Measures read throughput for the bulk-export path.
fn bench_full_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_scan");
    for &n in &[1_000, 10_000, 50_000] {
        let (tmp, store) = populate_store(n, (n / 32).max(1));

        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &store, |b, store| {
            b.iter(|| {
                black_box(store.iter_all_symbols().expect("iter_all_symbols"));
            });
        });
        black_box(&tmp);
    }
    group.finish();
}

/// Callers-of resolution: resolving who calls a given symbol exercises
/// the edges table and the `callers_of` query path.
fn bench_callers_of(c: &mut Criterion) {
    let mut group = c.benchmark_group("callers_of");
    for &n in &[500, 2_000] {
        let (tmp, store, _ids) = populate_with_edges(n);
        // The middle symbol has both incoming and outgoing edges.
        let target = format!("sym_{:05}", n / 2);

        group.bench_with_input(
            BenchmarkId::from_parameter(n),
            &(store, target),
            |b, (store, target)| {
                b.iter(|| {
                    black_box(store.callers_of(target).expect("callers_of"));
                });
            },
        );
        black_box(&tmp);
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_bulk_insert,
    bench_edge_insert,
    bench_cold_lookup,
    bench_warm_lookup,
    bench_full_scan,
    bench_callers_of,
    bench_pragma_sensitivity,
);
criterion_main!(benches);
