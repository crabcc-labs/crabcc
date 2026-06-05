//! Fuzzy + prefix search fuzz target.
//!
//! `Fts::fuzzy` runs a bounded Levenshtein scan and `Fts::prefix` a prefix
//! scan over the indexed name-ish fields. Both slice query and candidate
//! strings and apply a fast-bail cap; the "never drop exact fuzzy matches
//! past the cap" fix (721) and the FSST-decode panic in fuzzy hits (9ef669be)
//! both lived here. Both return `Result`, so an `Err` is fine — only a panic
//! (e.g. multi-byte boundary in the Lev matrix, or a cap-arithmetic overflow)
//! is a bug.
//!
//! The corpus is fixed; the fuzzer drives the query string and the limit. The
//! first byte is the cap (0 included — exercises the empty-budget edge) and
//! the rest is the query, taken UTF-8-lossy so multi-byte queries reach the
//! matcher.
//!
//! Build:  `cargo +nightly fuzz build`
//! Run:    `cargo +nightly fuzz run fts_fuzzy_prefix`

#![no_main]

use crabcc_core::fts::Fts;
use crabcc_core::{Symbol, SymbolKind};
use libfuzzer_sys::fuzz_target;

fn sym(name: &str) -> Symbol {
    Symbol {
        name: name.to_string(),
        kind: SymbolKind::Function,
        signature: None,
        parent: None,
        file: "src/lib.rs".to_string(),
        line_start: 1,
        line_end: 1,
        visibility: None,
    }
}

// Build the (cheap, Vec-backed) index once per worker process.
thread_local! {
    static FTS: Fts = Fts::from_symbols([
        sym("Store"),
        sym("open"),
        sym("compress"),
        sym("decompress"),
        sym("find_symbol"),
        sym("fuzzy_match"),
        sym("prefix_scan"),
        sym("Symbol"),
    ]);
}

fuzz_target!(|data: &[u8]| {
    let limit = data.first().copied().unwrap_or(8) as usize;
    let q = String::from_utf8_lossy(data.get(1..).unwrap_or(&[]));
    FTS.with(|f| {
        let _ = f.fuzzy(&q, limit);
        let _ = f.prefix(&q, limit);
    });
});
