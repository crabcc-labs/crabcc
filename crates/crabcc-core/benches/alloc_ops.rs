// Allocation-per-operation profiler for crabcc-core query hot paths.
//
// divan counts actual heap allocations per iteration alongside latency,
// surfacing regressions invisible to latency-only benchmarks (same time,
// more allocs = worse under memory pressure / mimalloc lock contention).
//
// Run:
//   cargo bench -p crabcc-core --bench alloc_ops --features bench
//   cargo bench -p crabcc-core --bench alloc_ops --features bench -- --sample-count 100

use crabcc_core::store::Store;
use crabcc_core::types::{Symbol, SymbolKind};
use divan::{black_box, AllocProfiler, Bencher};
use tempfile::TempDir;

#[global_allocator]
static ALLOC: AllocProfiler = AllocProfiler::system();

fn main() {
    divan::main();
}

const N: usize = 500;

fn synth_symbols(n: usize) -> Vec<Symbol> {
    (0..n)
        .map(|i| Symbol {
            name: format!("sym_{i:05}"),
            kind: if i % 2 == 0 { SymbolKind::Function } else { SymbolKind::Method },
            signature: Some(format!("fn sym_{i:05}() -> u32")),
            parent: (i % 3 != 0).then(|| format!("Mod{}", i % 17)),
            file: format!("src/module_{}.rs", i % 20),
            line_start: (i as u32) * 4 + 1,
            line_end: (i as u32) * 4 + 3,
            visibility: (i % 5 == 0).then_some("pub".into()),
        })
        .collect()
}

fn make_store() -> (Store, TempDir) {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");
    let store = Store::open(&db).unwrap();
    let syms = synth_symbols(N);
    // Group by the 20 synthetic files
    let mut by_file: std::collections::HashMap<usize, Vec<Symbol>> = Default::default();
    for s in syms {
        let bucket: usize = s.file.rsplit('_').next()
            .and_then(|t| t.trim_end_matches(".rs").parse().ok())
            .unwrap_or(0);
        by_file.entry(bucket).or_default().push(s);
    }
    for (bucket, syms) in by_file {
        let path = format!("src/module_{bucket}.rs");
        let fid = store.upsert_file(&path, "aabbccdd", 0, "rust").unwrap();
        store.replace_symbols(fid, &syms).unwrap();
    }
    (store, dir)
}

// Warm name lookup.  Target: ≤ 4 allocs (Vec, String rows).
#[divan::bench]
fn find_by_name_warm(bencher: Bencher) {
    let (store, _dir) = make_store();
    bencher.bench_local(|| {
        black_box(store.find_by_name("sym_00100").unwrap());
    });
}

// Name projection scan (SymbolName, no sig blob decompression).
// Alloc count tracks Vec reallocation as corpus grows.
#[divan::bench]
fn iter_symbol_names(bencher: Bencher) {
    let (store, _dir) = make_store();
    bencher.bench_local(|| {
        black_box(store.iter_symbol_names().unwrap());
    });
}

// Bulk insert at three scales.
#[divan::bench(args = [64, 256, 512])]
fn replace_symbols(bencher: Bencher, count: usize) {
    let syms = synth_symbols(count);
    let dir = TempDir::new().unwrap();
    let store = Store::open(&dir.path().join("i.db")).unwrap();
    let fid = store.upsert_file("src/f.rs", "00000000", 0, "rust").unwrap();
    bencher.bench_local(|| {
        black_box(store.replace_symbols(fid, black_box(&syms)).unwrap());
    });
}
