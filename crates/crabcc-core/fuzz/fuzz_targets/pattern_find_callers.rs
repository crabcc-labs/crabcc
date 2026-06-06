//! Tree-sitter caller-extraction fuzz target.
//!
//! `pattern::find_callers` parses arbitrary source with ast-grep (tree-sitter)
//! and runs two query patterns over the tree, collecting hits by (line, col).
//! Parsing attacker-shaped or truncated source and slicing out match spans is
//! a classic panic surface (byte/char-boundary math, ast-grep edge cases). It
//! returns `Vec<Hit>` — no `Result` — so any panic is a bug.
//!
//! The first byte selects the language so every wired tree-sitter grammar
//! (rust / go / python / js / ts / tsx / ruby) gets exercised; the rest is the
//! source. The needle is fixed — what we're fuzzing is the parse + extract,
//! not the name.
//!
//! Build:  `cargo +nightly fuzz build`
//! Run:    `cargo +nightly fuzz run pattern_find_callers`

#![no_main]

use crabcc_core::pattern;
use libfuzzer_sys::fuzz_target;

const LANGS: [&str; 7] = [
    "rust",
    "go",
    "python",
    "javascript",
    "typescript",
    "tsx",
    "ruby",
];

fuzz_target!(|data: &[u8]| {
    let Some((&sel, src_bytes)) = data.split_first() else {
        return;
    };
    let Some(lang) = pattern::lang_for(LANGS[sel as usize % LANGS.len()]) else {
        return;
    };
    let src = String::from_utf8_lossy(src_bytes);
    let _ = pattern::find_callers(&src, lang, "foo");
});
