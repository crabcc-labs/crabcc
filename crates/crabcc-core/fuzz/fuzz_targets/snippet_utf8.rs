//! Snippet UTF-8 truncation fuzz target — the exact path behind the
//! "UTF-8 panic in snippets" fix (9ef669be).
//!
//! `query::compact_snippet` collapses whitespace then truncates to the 80th
//! *char* (not byte). The original bug was `&one_line[..80]`, which panics
//! whenever byte 80 splits a multi-byte char — any non-ASCII line over 80
//! bytes. This target hammers that boundary with arbitrary multi-byte text.
//!
//! The function takes `&str`, so we drive it with a guaranteed-valid UTF-8
//! string (`Arbitrary for &str` yields a valid-UTF-8 slice of the input),
//! which keeps dense multi-byte sequences — exactly what stresses the
//! char-boundary math — in play.
//!
//! Contract: must never panic. Output is unconstrained, so no assertion.
//!
//! Build:  `cargo +nightly fuzz build`
//! Run:    `cargo +nightly fuzz run snippet_utf8`

#![no_main]

use crabcc_core::query;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|s: &str| {
    let _ = query::compact_snippet(s);
});
