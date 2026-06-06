//! Markdown parse + drawer-body sanitize fuzz target.
//!
//! `md::sanitize_drawer_body` runs on every memory-drawer body before it is
//! keyword-indexed, and `md::parse` underlies it. Both walk an AST and build
//! strings by slicing the input, so an off-by-one on a multi-byte UTF-8
//! boundary panics (`byte index N is not a char boundary`) — the same class
//! as the snippet UTF-8 panic fixed in 9ef669be. Both take `&str`, so input
//! is always valid UTF-8; `from_utf8_lossy` keeps multi-byte sequences in
//! play (where boundary bugs live) without discarding the fuzzer's bytes.
//!
//! Contract: neither call may panic, whatever the (valid-UTF-8) text is.
//! `md::parse` returns `Result`, so an `Err` is an uninteresting outcome —
//! only a panic is a bug.
//!
//! Build:  `cargo +nightly fuzz build`
//! Run:    `cargo +nightly fuzz run md_parse_sanitize`

#![no_main]

use crabcc_core::md;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    let _ = md::parse(&s);
    let _ = md::sanitize_drawer_body(&s);
});
