//! FSST + SQLite roundtrip fuzz target.
//!
//! The hypothesis under test: any byte sequence that survives `Codec::compress`
//! must come back byte-identical from `Codec::decompress` after a full SQLite
//! INSERT/SELECT cycle. SQLite stores the encoded payload as a TEXT blob
//! (since `signature` is `TEXT NOT NULL DEFAULT 0` in our schema; rusqlite
//! handles binary writes fine via `params!`).
//!
//! Strategy:
//!   1. Take fuzzer-supplied bytes (`data`).
//!   2. Skip empties — `Codec::train` rejects empty samples, and an empty
//!      input would short-circuit compress/decompress to empty without
//!      exercising any FSST state machine.
//!   3. Train a fresh codec on `data`'s 64-byte chunks (caps at 64 chunks so
//!      the fuzzer doesn't blow training time on a 10MB input).
//!   4. Encode `data`, INSERT into an in-memory SQLite using the real
//!      `schema/001_init.sql`, SELECT it back, decompress, assert eq.
//!
//! Build:  `cargo +nightly fuzz build`
//! Run:    `cargo +nightly fuzz run fsst_sqlite_roundtrip`

#![no_main]

use crabcc_core::compress::Codec;
use libfuzzer_sys::fuzz_target;
use rusqlite::{params, Connection};

/// Embedded so the fuzz binary is self-contained — `include_str!` is resolved
/// at compile time relative to *this* file, hence the four `..` segments to
/// reach the workspace's `schema/` directory.
const SCHEMA_SQL: &str = include_str!("../../../../schema/001_init.sql");

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    // Train on small chunks of the input itself. FSST is robust to weird
    // training data; if `train` errors we treat that as an uninteresting
    // fuzzer input rather than a bug, since the public API allows it.
    let chunks: Vec<&[u8]> = data.chunks(64).take(64).collect();
    let codec = match Codec::train(&chunks) {
        Ok(c) => c,
        Err(_) => return,
    };

    let encoded = codec.compress(data);

    // In-memory DB; `journal_mode=WAL` etc. inside the schema is a no-op for
    // `:memory:` so we ignore any "cannot change into wal mode" warnings.
    let conn = match Connection::open_in_memory() {
        Ok(c) => c,
        Err(_) => return,
    };
    if conn.execute_batch(SCHEMA_SQL).is_err() {
        return;
    }

    // Insert a parent file row first to satisfy the FK on `symbols.file_id`.
    let fid: i64 = match conn.query_row(
        "INSERT INTO files(path, sha256, mtime, lang, indexed_at)
         VALUES('a','h',0,'rust',0) RETURNING id",
        [],
        |row| row.get(0),
    ) {
        Ok(id) => id,
        Err(_) => return,
    };

    if conn
        .execute(
            "INSERT INTO symbols
             (file_id, name, kind, signature, parent, line_start, line_end, visibility, signature_enc)
             VALUES (?1, 'x', 'function', ?2, NULL, 1, 1, NULL, 1)",
            params![fid, encoded],
        )
        .is_err()
    {
        return;
    }

    let bytes: Vec<u8> = match conn.query_row(
        "SELECT signature FROM symbols WHERE file_id = ?1",
        params![fid],
        |row| row.get(0),
    ) {
        Ok(b) => b,
        Err(_) => return,
    };

    let decoded = codec.decompress(&bytes);
    assert_eq!(decoded, data, "fsst+sqlite roundtrip diverged");
});
