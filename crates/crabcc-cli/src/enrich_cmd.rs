//! `crabcc enrich <query>` — token-bounded, cached documentation context
//! pulled from the memory Palace (populated by `crabcc crawl --remember`).
//!
//! Built for prompt context-gathering / initial search: it returns the
//! single most-relevant cached concept plus one *semi-relevant* one (the
//! best hit from a different room, for a bit of breadth), trimmed to a
//! token budget (~2000 by default). The assembled context is cached on
//! disk keyed by `(query, budget)`, so a repeat ask is a file read rather
//! than another search.
//!
//! Token counting is a deliberate char-based approximation (`CHARS_PER_TOKEN`)
//! — no tokenizer dep; the budget is a soft guardrail, not an exact count.

use anyhow::Result;
use crabcc_memory::{DrawerHit, Palace};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Rough chars-per-token for English prose. Keeps the budget tokenizer-free.
const CHARS_PER_TOKEN: usize = 4;
/// Default token budget when `--max-tokens` is 0/unset.
const DEFAULT_MAX_TOKENS: usize = 2000;
/// How many candidates to pull from the Palace before selecting.
const SEARCH_LIMIT: usize = 8;

pub fn run(root: &Path, query: &str, max_tokens: usize) -> Result<()> {
    let budget = if max_tokens == 0 {
        DEFAULT_MAX_TOKENS
    } else {
        max_tokens
    };
    let key = cache_key(query, budget);
    let cache_path = root.join(".crabcc").join("enrich-cache").join(&key);

    // Cache hit: return the previously-assembled context verbatim.
    if let Ok(cached) = std::fs::read_to_string(&cache_path) {
        print!("{cached}");
        return Ok(());
    }

    let palace = Palace::open(root)?;
    let hits = palace.search(query, SEARCH_LIMIT)?.hits;
    let selected = select_relevant(&hits);
    if selected.is_empty() {
        eprintln!(
            "enrich: nothing cached for {query:?} — crawl some docs first \
             (e.g. `crabcc crawl <url> --remember`)"
        );
        return Ok(());
    }

    let context = assemble(&selected, budget);
    // Best-effort cache write; never fail the command on a cache miss.
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&cache_path, &context);

    print!("{context}");
    Ok(())
}

/// Deterministic cache filename for a `(query, budget)` pair.
fn cache_key(query: &str, budget: usize) -> String {
    use std::fmt::Write as _;
    let mut h = Sha256::new();
    h.update(budget.to_le_bytes());
    h.update([0]); // domain separator
    h.update(query.as_bytes());
    let digest = h.finalize();
    let mut key = String::with_capacity(digest.len() * 2 + 3);
    for b in digest {
        let _ = write!(key, "{b:02x}");
    }
    key.push_str(".md");
    key
}

/// Pick the most-relevant hit plus one *semi-relevant* one — the best hit
/// from a different room (breadth), falling back to the next hit when every
/// candidate shares the top room. Returns 0, 1, or 2 hits.
fn select_relevant(hits: &[DrawerHit]) -> Vec<&DrawerHit> {
    let mut out = Vec::new();
    let Some(top) = hits.first() else {
        return out;
    };
    out.push(top);
    let semi = hits
        .iter()
        .skip(1)
        .find(|h| h.room != top.room)
        .or_else(|| hits.get(1));
    if let Some(s) = semi {
        out.push(s);
    }
    out
}

/// Concatenate the selected drawers into a single context block, trimmed to
/// roughly `max_tokens`. The top hit gets the larger share (~2/3) so the
/// primary concept dominates; the semi-relevant hit gets the remainder.
fn assemble(selected: &[&DrawerHit], max_tokens: usize) -> String {
    let total_chars = max_tokens.saturating_mul(CHARS_PER_TOKEN);
    let n = selected.len();
    let mut out = String::new();
    for (i, h) in selected.iter().enumerate() {
        let share = match (i, n) {
            (_, 1) => total_chars,         // sole hit gets the whole budget
            (0, _) => total_chars * 2 / 3, // primary concept
            _ => total_chars / 3,          // semi-relevant
        };
        let room = h.room.as_deref().unwrap_or("-");
        let header = format!("## {} (room={room}, score={:.2})\n", h.source_id, h.score);
        out.push_str(&header);
        let body_budget = share.saturating_sub(header.len());
        let body = truncate_on_char_boundary(&h.body, body_budget);
        out.push_str(body);
        if body.len() < h.body.len() {
            out.push_str("\n…[truncated]");
        }
        out.push_str("\n\n");
    }
    out
}

/// Truncate `s` to at most `max_bytes`, backing up to the nearest char
/// boundary so the result is always valid UTF-8.
fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(source: &str, room: Option<&str>, score: f32, body: &str) -> DrawerHit {
        DrawerHit {
            id: 0,
            score,
            source_id: source.into(),
            body: body.into(),
            wing: "crawl".into(),
            room: room.map(String::from),
        }
    }

    #[test]
    fn selects_top_plus_a_different_room() {
        let hits = vec![
            hit("a", Some("rustlang.org"), 0.9, "primary"),
            hit("b", Some("rustlang.org"), 0.8, "same room"),
            hit("c", Some("docs.rs"), 0.7, "other room"),
        ];
        let sel = select_relevant(&hits);
        assert_eq!(sel.len(), 2);
        assert_eq!(sel[0].source_id, "a");
        assert_eq!(sel[1].source_id, "c"); // first hit from a *different* room
    }

    #[test]
    fn falls_back_to_next_hit_when_all_same_room() {
        let hits = vec![hit("a", Some("x"), 0.9, "p"), hit("b", Some("x"), 0.8, "q")];
        let sel = select_relevant(&hits);
        assert_eq!(sel.len(), 2);
        assert_eq!(sel[1].source_id, "b");
    }

    #[test]
    fn single_and_empty() {
        assert_eq!(select_relevant(&[]).len(), 0);
        let one = vec![hit("a", None, 1.0, "only")];
        assert_eq!(select_relevant(&one).len(), 1);
    }

    #[test]
    fn assemble_respects_budget_and_truncates() {
        let big = "x".repeat(100_000);
        let hits = vec![
            hit("a", Some("r1"), 0.9, &big),
            hit("b", Some("r2"), 0.5, &big),
        ];
        let sel = select_relevant(&hits);
        let out = assemble(&sel, 100); // 100 tokens → ~400 chars
                                       // Soft budget: output stays in the same ballpark, not 200k chars.
        assert!(out.len() < 100 * CHARS_PER_TOKEN + 200, "len={}", out.len());
        assert!(out.contains("## a"));
        assert!(out.contains("## b"));
        assert!(out.contains("…[truncated]"));
    }

    #[test]
    fn cache_key_is_stable_and_budget_sensitive() {
        assert_eq!(cache_key("q", 2000), cache_key("q", 2000));
        assert_ne!(cache_key("q", 2000), cache_key("q", 1000));
        assert_ne!(cache_key("q", 2000), cache_key("other", 2000));
        assert!(cache_key("q", 2000).ends_with(".md"));
    }

    #[test]
    fn truncate_keeps_utf8_boundaries() {
        let s = "héllo wörld"; // multi-byte chars
        let t = truncate_on_char_boundary(s, 2); // mid 'é' → back up to 'h'
        assert_eq!(t, "h");
        assert_eq!(truncate_on_char_boundary(s, 1000), s);
    }
}
