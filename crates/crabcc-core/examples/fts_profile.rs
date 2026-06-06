//! Profiling driver for the native fuzzy/prefix search. Build + run a fixed
//! workload so a profiler (callgrind/samply) attributes cost to the hot path.
//!
//!   cargo build --release --example fts_profile -p crabcc-core
//!   valgrind --tool=callgrind target/release/examples/fts_profile
//!
//! Names are snake_case (multi-token) so the token-matching path is exercised.

use crabcc_core::fts::Fts;
use crabcc_core::types::{Symbol, SymbolKind};
use std::hint::black_box;

fn synth(n: usize) -> Vec<Symbol> {
    (0..n)
        .map(|i| Symbol {
            name: format!("get_user_profile_{i:05}"),
            kind: if i % 2 == 0 {
                SymbolKind::Function
            } else {
                SymbolKind::Method
            },
            signature: None,
            parent: None,
            file: format!("src/path_{}.rs", i % 64),
            line_start: i as u32,
            line_end: i as u32,
            visibility: None,
        })
        .collect()
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50_000);
    let symbols = synth(n);

    // Build phase (the CLI pays this on every invocation).
    for _ in 0..3 {
        black_box(Fts::from_symbols(symbols.iter().cloned()));
    }

    let fts = Fts::from_symbols(symbols);
    // Fuzzy phase: a mix of exact, typo, and no-match queries.
    let fuzzy_qs = [
        "user",
        "usr",
        "porfile",
        "get_user_profile_25000",
        "zzzzzzz",
    ];
    for _ in 0..20 {
        for q in fuzzy_qs {
            black_box(fts.fuzzy(q, 20).unwrap());
        }
    }
    // Prefix phase.
    for _ in 0..50 {
        black_box(fts.prefix("user", 20).unwrap());
        black_box(fts.prefix("get", 20).unwrap());
    }
}
