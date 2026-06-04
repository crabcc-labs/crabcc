// Criterion micro-benches for the native (SQLite-backed) fuzzy + prefix
// symbol-name search that replaced the Tantivy sidecar.
//
// Gated behind the `bench` feature so CI's `--all-targets` skips it:
//   cargo bench -p crabcc-core --features bench --bench fts_search
//
// The matrix sweeps corpus size × query shape so we can see how the
// brute-force Levenshtein / prefix scan scales:
//
//   * fts_build           — Fts::from_symbols construction cost (the CLI pays
//                           this on every `crabcc fuzzy` / `prefix` invocation).
//   * fuzzy/exact         — query == an existing name (distance 0).
//   * fuzzy/typo1         — one edit away from a dense cluster of names.
//   * fuzzy/typo2         — two edits away (max budget; least early-exit).
//   * fuzzy/nomatch       — far from everything (best case: early-exit fires).
//   * prefix/broad        — matches the whole corpus, then truncates.
//   * prefix/narrow       — matches a handful.
//
// Corpus sizes bracket real repos: 1k (small crate), 10k (this repo),
// 50k (mc-mothership-scale). Names are dense `sym_NNNNN` so the fuzzy scan
// can't trivially early-exit — this is the realistic worst case.

use crabcc_core::fts::Fts;
use crabcc_core::types::{Symbol, SymbolKind};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

const SIZES: &[usize] = &[1_000, 10_000, 50_000];
const LIMIT: usize = 20;

/// Deterministic synthetic corpus: `sym_00000`..`sym_NNNNN`, alternating
/// function/method, spread across 64 synthetic files.
fn synth_symbols(n: usize) -> Vec<Symbol> {
    (0..n)
        .map(|i| Symbol {
            name: format!("sym_{i:05}"),
            kind: if i % 2 == 0 {
                SymbolKind::Function
            } else {
                SymbolKind::Method
            },
            signature: None,
            parent: (i % 3 != 0).then(|| format!("Mod{}", i % 17)),
            file: format!("synthetic/path_{}.rs", i % 64),
            line_start: (i as u32) * 4 + 1,
            line_end: (i as u32) * 4 + 3,
            visibility: (i % 5 == 0).then_some("pub".into()),
        })
        .collect()
}

/// Query the middle of the corpus so prefix/fuzzy hits land mid-table.
fn target(n: usize) -> String {
    format!("sym_{:05}", n / 2)
}

fn bench_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("fts_build");
    for &n in SIZES {
        let symbols = synth_symbols(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &symbols, |b, syms| {
            b.iter(|| {
                let fts = Fts::from_symbols(syms.iter().cloned());
                black_box(fts);
            })
        });
    }
    group.finish();
}

// (label, derive query from the mid-corpus target name).
type Shape = (&'static str, fn(&str) -> String);

fn bench_fuzzy(c: &mut Criterion) {
    let shapes: &[Shape] = &[
        ("exact", |t| t.to_string()),
        // One digit flipped — still distance 1 from a dense cluster.
        ("typo1", |t| {
            let mut s = t.to_string();
            s.replace_range(4..5, "9");
            s
        }),
        // Two chars flipped — exercises the full edit budget.
        ("typo2", |t| {
            let mut s = t.to_string();
            s.replace_range(4..6, "99");
            s
        }),
        ("nomatch", |_| "zzzzzzzzzz".to_string()),
        // Matches the `sym` token of *every* row at distance 0 — the dense
        // case the fast-bail targets (should stay flat as the corpus grows).
        ("dense", |_| "sym".to_string()),
    ];

    let mut group = c.benchmark_group("fuzzy");
    for &n in SIZES {
        let fts = Fts::from_symbols(synth_symbols(n));
        let t = target(n);
        group.throughput(Throughput::Elements(n as u64));
        for (label, derive) in shapes {
            let q = derive(&t);
            group.bench_with_input(BenchmarkId::new(*label, n), &q, |b, q| {
                b.iter(|| {
                    let hits = fts.fuzzy(q, LIMIT).expect("fuzzy");
                    black_box(hits);
                })
            });
        }
    }
    group.finish();
}

fn bench_prefix(c: &mut Criterion) {
    let mut group = c.benchmark_group("prefix");
    for &n in SIZES {
        let fts = Fts::from_symbols(synth_symbols(n));
        group.throughput(Throughput::Elements(n as u64));
        // "broad" matches every symbol (worst case: filter + sort all N);
        // "narrow" matches a handful around the mid-corpus target.
        let broad = "sym_".to_string();
        let narrow = {
            let t = target(n);
            t[..t.len() - 1].to_string()
        };
        for (label, q) in [("broad", broad), ("narrow", narrow)] {
            group.bench_with_input(BenchmarkId::new(label, n), &q, |b, q| {
                b.iter(|| {
                    let hits = fts.prefix(q, LIMIT).expect("prefix");
                    black_box(hits);
                })
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_build, bench_fuzzy, bench_prefix);
criterion_main!(benches);
