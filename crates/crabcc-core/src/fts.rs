// Native SQLite-backed fuzzy + prefix search over symbol names.
//
// Replaces the former Tantivy sidecar (`.crabcc/tantivy/`). Symbol names are
// loaded straight from the live SQLite index via `Store::iter_all_symbols`, so
// fuzzy/prefix always reflect the *current* index — there is no separate
// rebuild step and no staleness window (the old sidecar went stale after
// `crabcc refresh`, which never rebuilt it).
//
// - `fuzzy`  = bounded Levenshtein (distance ≤ 2), computed in Rust with an
//   early-exit banded DP that bails the moment a row can't stay within budget.
// - `prefix` = case-insensitive prefix match.
//
// Both are linear in the symbol count, which is comfortably fast at the
// tens-of-thousands scale crabcc targets (a brute-force pass over ~38k short
// names is sub-millisecond). Dropping Tantivy removes ~20 transitive crates
// from the build.

use crate::store::Store;
use crate::types::SymbolKind;
use anyhow::Result;
use serde::Serialize;
use std::cmp::Ordering;

/// In-memory view of the indexed symbols, ready for name lookups.
pub struct Fts {
    rows: Vec<Row>,
}

/// One searchable symbol. `name_lower` is precomputed so queries don't
/// re-lowercase the whole corpus on every call.
struct Row {
    name: String,
    name_lower: String,
    kind: &'static str,
    file: String,
    line: u64,
    parent: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FuzzyHit {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u64,
    pub parent: Option<String>,
    pub score: f32,
}

/// Maximum edit distance for `fuzzy` — matches the old Tantivy
/// `FuzzyTermQuery::new(term, 2, _)` budget.
const MAX_EDIT_DISTANCE: usize = 2;

impl Fts {
    /// Build the in-memory index from the live SQLite store. Cheap — a single
    /// `iter_all_symbols` scan and a `to_lowercase` per name.
    pub fn from_store(store: &Store) -> Result<Self> {
        Ok(Self::from_symbols(store.iter_all_symbols()?))
    }

    /// Build directly from in-memory symbols, bypassing SQLite. `from_store`
    /// delegates here; benches and perf-guard tests use it to spin up large
    /// synthetic corpora without paying the indexing cost.
    pub fn from_symbols(symbols: impl IntoIterator<Item = crate::types::Symbol>) -> Self {
        let rows = symbols
            .into_iter()
            .map(|s| Row {
                name_lower: s.name.to_lowercase(),
                kind: kind_str(s.kind),
                file: s.file,
                line: s.line_start as u64,
                parent: s.parent.filter(|p| !p.is_empty()),
                name: s.name,
            })
            .collect();
        Self { rows }
    }

    /// Number of searchable symbols.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Typo-tolerant lookup: every symbol whose name is within Levenshtein
    /// distance `MAX_EDIT_DISTANCE` of `query`, ranked closest-first.
    pub fn fuzzy(&self, query: &str, limit: usize) -> Result<Vec<FuzzyHit>> {
        let q = query.to_lowercase();
        let mut scored: Vec<(usize, &Row)> = Vec::new();
        for row in &self.rows {
            if let Some(d) = bounded_levenshtein(&q, &row.name_lower, MAX_EDIT_DISTANCE) {
                scored.push((d, row));
            }
        }
        // Closest match first; ties broken by name for stable output.
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
        scored.truncate(limit);
        Ok(scored
            .into_iter()
            // distance 0 → 1.0, 1 → 0.5, 2 → 0.33 — higher means closer.
            .map(|(d, r)| r.to_hit(1.0 / (1.0 + d as f32)))
            .collect())
    }

    /// Completion-style lookup: every symbol whose (lowercased) name starts
    /// with `query`, shortest name first (closest to the prefix).
    pub fn prefix(&self, query: &str, limit: usize) -> Result<Vec<FuzzyHit>> {
        let q = query.to_lowercase();
        let mut hits: Vec<&Row> = self
            .rows
            .iter()
            .filter(|r| r.name_lower.starts_with(&q))
            .collect();
        hits.sort_by(|a, b| {
            a.name_lower
                .len()
                .cmp(&b.name_lower.len())
                .then_with(|| a.name.cmp(&b.name))
        });
        hits.truncate(limit);
        Ok(hits
            .into_iter()
            .map(|r| {
                // Ratio of query length to name length: 1.0 for an exact hit,
                // smaller the more the name overshoots the prefix.
                let score = if r.name_lower.is_empty() {
                    0.0
                } else {
                    q.len() as f32 / r.name_lower.len() as f32
                };
                r.to_hit(score)
            })
            .collect())
    }
}

impl Row {
    fn to_hit(&self, score: f32) -> FuzzyHit {
        FuzzyHit {
            name: self.name.clone(),
            kind: self.kind.to_string(),
            file: self.file.clone(),
            line: self.line,
            parent: self.parent.clone(),
            score,
        }
    }
}

/// Levenshtein distance between `a` and `b`, bounded by `max`. Returns `None`
/// as soon as it's provable that the distance exceeds `max` (length gap too
/// large, or every cell in a DP row already over budget), which lets the
/// common "no match" case bail early.
fn bounded_levenshtein(a: &str, b: &str, max: usize) -> Option<usize> {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (la, lb) = (a.len(), b.len());
    if la.abs_diff(lb) > max {
        return None;
    }
    // Classic two-row DP. `prev[j]` = edit distance between a[..i-1] and b[..j].
    let mut prev: Vec<usize> = (0..=lb).collect();
    let mut cur: Vec<usize> = vec![0; lb + 1];
    for i in 1..=la {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=lb {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(cur[j]);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    match prev[lb].cmp(&max) {
        Ordering::Greater => None,
        _ => Some(prev[lb]),
    }
}

fn kind_str(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::Function => "function",
        SymbolKind::Method => "method",
        SymbolKind::Class => "class",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Interface => "interface",
        SymbolKind::Const => "const",
        SymbolKind::Var => "var",
        SymbolKind::Type => "type",
        SymbolKind::Macro => "macro",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::build_index;

    fn fixture() -> (tempfile::TempDir, Store, Fts) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("a.ts"),
            "export function getUserProfile(){};\n\
             export function getUserAvatar(){};\n\
             export class UserSession {};\n\
             export type Settings = {};\n",
        )
        .unwrap();
        std::fs::write(
            root.join("b.rb"),
            "class Authenticator\n  def authenticate; end\nend\n",
        )
        .unwrap();
        let store = Store::open(&root.join("idx.db")).unwrap();
        build_index(root, &store).unwrap();
        let fts = Fts::from_store(&store).unwrap();
        assert!(fts.len() >= 5, "expected ≥5 symbols, got {}", fts.len());
        (dir, store, fts)
    }

    #[test]
    fn prefix_finds_user_symbols() {
        let (_dir, _store, fts) = fixture();
        let hits = fts.prefix("getUser", 10).unwrap();
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(
            names.iter().any(|n| n.starts_with("getUser")),
            "got: {names:?}"
        );
    }

    #[test]
    fn prefix_is_case_insensitive() {
        let (_dir, _store, fts) = fixture();
        let hits = fts.prefix("getuser", 10).unwrap();
        assert!(
            hits.iter().any(|h| h.name.starts_with("getUser")),
            "lowercased prefix should still match camelCase names"
        );
    }

    #[test]
    fn fuzzy_tolerates_typo() {
        // "Authentcator" missing an 'i' — Levenshtein distance 1.
        let (_dir, _store, fts) = fixture();
        let hits = fts.fuzzy("Authentcator", 10).unwrap();
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(
            names.iter().any(|n| n.contains("Authenticator")),
            "fuzzy should match Authenticator, got: {names:?}"
        );
    }

    #[test]
    fn fuzzy_returns_score() {
        let (_dir, _store, fts) = fixture();
        let hits = fts.fuzzy("UserSession", 5).unwrap();
        assert!(hits.iter().any(|h| h.score > 0.0));
    }

    #[test]
    fn fuzzy_rejects_distant_names() {
        let (_dir, _store, fts) = fixture();
        // "Settings" is > distance 2 from "Authenticator", so it must not
        // surface when we fuzzy-search the latter.
        let hits = fts.fuzzy("Authenticator", 10).unwrap();
        assert!(
            !hits.iter().any(|h| h.name == "Settings"),
            "distant names must be filtered out"
        );
    }

    #[test]
    fn from_store_is_idempotent() {
        let (_dir, store, _fts) = fixture();
        let a = Fts::from_store(&store).unwrap().len();
        let b = Fts::from_store(&store).unwrap().len();
        assert_eq!(a, b);
    }

    #[test]
    fn bounded_levenshtein_matches_known_distances() {
        assert_eq!(bounded_levenshtein("store", "store", 2), Some(0));
        assert_eq!(bounded_levenshtein("strore", "store", 2), Some(1));
        assert_eq!(bounded_levenshtein("kitten", "sitting", 2), None); // distance 3
        assert_eq!(bounded_levenshtein("kitten", "sitten", 2), Some(1));
        assert_eq!(bounded_levenshtein("", "ab", 2), Some(2));
        assert_eq!(bounded_levenshtein("abc", "xyz", 2), None); // distance 3
    }
}
