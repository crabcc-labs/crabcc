//! Pure encoding helpers for the SQLite backend — timestamps, FTS5
//! match-string normalisation, and f32-vector ↔ blob round-trip.
//!
//! Free of any rusqlite connection state; safe to call from anywhere
//! the backend's hot path needs them.

/// Wall-clock seconds since the epoch, narrowed to i64 to match the
/// schema's `created_at` column type.
pub(super) fn now_secs() -> i64 {
    crabcc_core::time::unix_now_secs() as i64
}

/// Build an FTS5 MATCH expression from raw user input. We tokenize on
/// non-alphanumerics + apostrophes, drop empties, and OR the terms — that
/// way "fox jumps" matches drawers containing either word, mirroring how
/// most search UIs work. Bare user text would be parsed as an FTS query
/// (and `'` / `"` could cause syntax errors), so this normalisation also
/// hardens the path against accidental query-string injection.
pub(super) fn fts_match_string(input: &str) -> String {
    // Capacity hint: most user queries are 1-4 tokens. Starting at 8
    // skips the early Vec doublings (4 → 8) for the common case while
    // costing ~64 B of stack on the rare long-query path.
    let mut terms: Vec<String> = Vec::with_capacity(8);
    for word in input
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
    {
        // Wrap each token in double quotes so FTS5 treats it as a literal
        // phrase rather than parsing internal apostrophes / digits.
        let mut buf = String::with_capacity(word.len() + 2);
        buf.push('"');
        for ch in word.chars() {
            if ch == '"' {
                buf.push_str("\"\"");
            } else {
                buf.push(ch);
            }
        }
        buf.push('"');
        terms.push(buf);
    }
    if terms.is_empty() {
        // Empty MATCH would be an FTS5 syntax error; substitute a token
        // that cannot exist in any drawer body. Returns zero rows cleanly.
        return "\"\u{e000}\"".into();
    }
    terms.join(" OR ")
}

// Only the sqlite-vec mirror (memory-vec) encodes f32 → blob directly; the
// default write path goes through `encode_embedding`.
#[cfg(feature = "memory-vec")]
pub(super) fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub(super) fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Decode a `drawer_embeddings` row into an f32 vector, honoring `quant`:
/// 0 = raw f32 LE, 1 = symmetric int8 (`crate::quant`). Centralized so the
/// brute-force scan and the sqlite-vec backfill share identical semantics —
/// the same role `body_from_row` plays for `body_enc`.
pub(super) fn decode_embedding(bytes: &[u8], quant: i64) -> Vec<f32> {
    if quant == 1 {
        crate::quant::dequantize_i8(bytes)
    } else {
        blob_to_vec(bytes)
    }
}

/// Encode an embedding for storage in `drawer_embeddings(bytes, quant)`.
///
/// Returns `(f32 blob, 0)` on `memory-vec` builds. Without `memory-vec`, the
/// brute-force-only build quantizes: int8 (`quant=1`, ~3.96x) by default, or
/// 1-bit binary (`quant=2`, 32x) when `CRABCC_EMBED_QUANT=binary` is set —
/// the memory-bound edge option, which trades recall for footprint. The gate
/// is deliberate: with `memory-vec` on, `bytes` is mirrored verbatim into the
/// sqlite-vec `FLOAT[384]` table, which would misread quantized bytes, so
/// quantized rows are only ever produced by the brute-force-only build.
#[cfg(not(feature = "memory-vec"))]
pub(super) fn encode_embedding(v: &[f32]) -> (Vec<u8>, i64) {
    if std::env::var("CRABCC_EMBED_QUANT").as_deref() == Ok("binary") {
        (crate::quant::quantize_binary(v), 2)
    } else {
        (crate::quant::quantize_i8(v), 1)
    }
}

#[cfg(feature = "memory-vec")]
pub(super) fn encode_embedding(v: &[f32]) -> (Vec<u8>, i64) {
    (vec_to_blob(v), 0)
}
