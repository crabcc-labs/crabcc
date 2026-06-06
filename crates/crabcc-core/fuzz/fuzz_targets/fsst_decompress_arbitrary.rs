//! FSST decompress-on-arbitrary-bytes fuzz target.
//!
//! The `fsst_sqlite_roundtrip` target only ever decodes bytes it just
//! produced via `compress`, so it can't reach the malformed-stream paths.
//! In production `Codec::decompress` is called on bytes read back from
//! SQLite, which can be truncated, corrupted, or encoded by a *different*
//! codec (the "FSST-signature decode in graph queries" panic, fixed in
//! 9ef669be, was exactly this). Its signature returns `Vec<u8>`, not a
//! `Result`, so the contract is: **decode must never panic, whatever the
//! input bytes are.** This target asserts that contract.
//!
//! Strategy:
//!   1. Train a well-formed codec on a fixed, repetitive code-like corpus
//!      (FSST learns nothing from random bytes — repetition is required for
//!      a non-trivial symbol table).
//!   2. Hand the *fuzzer's* arbitrary bytes straight to `decompress`.
//!   3. Success == returns without panicking. The output is meaningless for
//!      bytes the codec didn't produce, so we deliberately make no
//!      roundtrip assertion.
//!
//! Build:  `cargo +nightly fuzz build`
//! Run:    `cargo +nightly fuzz run fsst_decompress_arbitrary`

#![no_main]

use crabcc_core::compress::Codec;
use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;

/// Repetitive, code-shaped training samples so the trained symbol table is
/// realistic. Trained once and reused across iterations — training is
/// deterministic and the codec is immutable, so amortizing it keeps the hot
/// loop on the actual code under test (`decompress`).
fn codec() -> &'static Codec {
    static CODEC: OnceLock<Codec> = OnceLock::new();
    CODEC.get_or_init(|| {
        let phrases: [&[u8]; 10] = [
            b"fn foo(x: u32) -> u32",
            b"fn bar(y: &str) -> String",
            b"impl Display for Foo",
            b"impl Debug for Foo",
            b"pub struct Bar { name: String }",
            b"pub enum Kind { A, B, C }",
            b"async fn handler(req: Request) -> Response",
            b"fn main() -> anyhow::Result<()>",
            b"let mut x = Vec::new();",
            b"use std::collections::HashMap;",
        ];
        Codec::train(&phrases).expect("fixed code-like corpus must train")
    })
}

fuzz_target!(|data: &[u8]| {
    // Contract under test: decode of arbitrary bytes must not panic.
    let _ = codec().decompress(data);
});
