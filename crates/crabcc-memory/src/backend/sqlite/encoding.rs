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

pub(super) fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

pub(super) fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}
