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

/// In-memory view of the indexed symbols, ready for name lookups.
pub struct Fts {
    rows: Vec<Row>,
}

/// One searchable symbol. `name_lower` is precomputed so queries don't
/// re-lowercase the whole corpus on every call; `tokens` holds the
/// alphanumeric segments so prefix/fuzzy can match *within* a snake_case or
/// dotted name (e.g. `profile` → `get_user_profile`), as the old tokenized
/// Tantivy index did.
struct Row {
    name: String,
    name_lower: String,
    tokens: Vec<String>,
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

/// Maximum edit distance for `fuzzy`. The old Tantivy index used
/// `FuzzyTermQuery::new(term, 2, true)` — Damerau, where an adjacent
/// transposition is a single edit. This is plain Levenshtein, so a
/// transposition costs 2; a single transposition therefore still matches
/// (it's exactly at the budget), and only transposition-plus-another-edit
/// drops out relative to the old behavior. Distance ≤ 2.
const MAX_EDIT_DISTANCE: usize = 2;

/// Fuzzy stops scanning once it has gathered `max(limit * FUZZY_SCAN_FACTOR,
/// FUZZY_SCAN_FLOOR)` candidates (or `limit` exact, distance-0 hits). This
/// bounds latency when a short/common query matches a large slice of the
/// corpus — the dominant cost there is computing the DP for every matching
/// row, and once the result `limit` can be filled there's no point continuing.
/// The trade-off is approximate ranking *only* in that plentiful-match case
/// (the top `limit` come from the rows scanned before the bail, not a global
/// ranking); when matches are sparse the cap is never hit and results are
/// exact. The floor keeps a healthy pool even for small `limit`s.
const FUZZY_SCAN_FACTOR: usize = 32;
const FUZZY_SCAN_FLOOR: usize = 512;

impl Fts {
    /// Build the in-memory index from the live SQLite store. Uses the name-only
    /// projection (`iter_symbol_names`) so the FSST-compressed `signature`
    /// column is never fetched or decoded — fuzzy/prefix only need the name.
    pub fn from_store(store: &Store) -> Result<Self> {
        let rows = store
            .iter_symbol_names()?
            .into_iter()
            .map(|s| Row::build(s.name, kind_str(s.kind), s.parent, s.file, s.line_start))
            .collect();
        Ok(Self { rows })
    }

    /// Build directly from full in-memory symbols, bypassing SQLite. Benches
    /// and perf-guard tests use it to spin up large synthetic corpora without
    /// paying the indexing cost. (`signature` is ignored — only the name-ish
    /// fields are indexed.)
    pub fn from_symbols(symbols: impl IntoIterator<Item = crate::types::Symbol>) -> Self {
        let rows = symbols
            .into_iter()
            .map(|s| Row::build(s.name, kind_str(s.kind), s.parent, s.file, s.line_start))
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

    /// Typo-tolerant lookup: every symbol within Levenshtein distance
    /// `MAX_EDIT_DISTANCE` of `query` — measured against the whole name *or*
    /// any one of its tokens, so a typo in a single segment of a snake_case /
    /// dotted name still matches. Ranked closest-first.
    pub fn fuzzy(&self, query: &str, limit: usize) -> Result<Vec<FuzzyHit>> {
        let q = query.to_lowercase();
        // One scratch reused across every row + token comparison in this scan.
        let mut lev = Lev::default();
        let mut scored: Vec<(usize, &Row)> = Vec::new();
        // Fast-bail thresholds (see FUZZY_SCAN_* docs): `limit` exact hits is
        // already an optimal top-`limit`, and a full candidate pool is enough
        // to fill `limit` after sorting.
        let cap = limit
            .saturating_mul(FUZZY_SCAN_FACTOR)
            .max(FUZZY_SCAN_FLOOR);
        let mut exact = 0usize;
        for row in &self.rows {
            let mut best = lev.distance(&q, &row.name_lower, MAX_EDIT_DISTANCE);
            for t in &row.tokens {
                if best == Some(0) {
                    break;
                }
                if let Some(d) = lev.distance(&q, t, MAX_EDIT_DISTANCE) {
                    best = Some(best.map_or(d, |b| b.min(d)));
                }
            }
            if let Some(d) = best {
                if d == 0 {
                    exact += 1;
                }
                scored.push((d, row));
                // Bail once we can't do better (`limit` exact hits) or we have
                // a full pool — bounds latency on dense-match queries.
                if exact >= limit || scored.len() >= cap {
                    break;
                }
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

    /// Completion-style lookup: every symbol whose whole name *or* one of its
    /// tokens starts with `query` (so `prefix("profile")` finds
    /// `get_user_profile`). Shortest matched unit first (closest to the query).
    pub fn prefix(&self, query: &str, limit: usize) -> Result<Vec<FuzzyHit>> {
        let q = query.to_lowercase();
        let mut hits: Vec<(usize, &Row)> = Vec::new();
        for row in &self.rows {
            // Length of the shortest unit (whole name or token) the query is a
            // prefix of — `None` if nothing matches.
            let mut matched = row
                .name_lower
                .starts_with(&q)
                .then_some(row.name_lower.len());
            for t in &row.tokens {
                if t.starts_with(&q) {
                    matched = Some(matched.map_or(t.len(), |m| m.min(t.len())));
                }
            }
            if let Some(mlen) = matched {
                hits.push((mlen, row));
            }
        }
        // Shortest matched unit first (closest to the query); tie by name.
        hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
        hits.truncate(limit);
        Ok(hits
            .into_iter()
            .map(|(mlen, r)| {
                // Ratio of query length to matched-unit length: 1.0 for an
                // exact hit, smaller the more the unit overshoots the prefix.
                let score = if mlen == 0 {
                    0.0
                } else {
                    q.len() as f32 / mlen as f32
                };
                r.to_hit(score)
            })
            .collect())
    }
}

impl Row {
    /// Construct a row, precomputing the lowercased name and dropping an empty
    /// parent. Shared by both the store-backed and in-memory build paths.
    fn build(
        name: String,
        kind: &'static str,
        parent: Option<String>,
        file: String,
        line_start: u32,
    ) -> Row {
        let name_lower = name.to_lowercase();
        let tokens = name_tokens(&name_lower);
        Row {
            name,
            name_lower,
            tokens,
            kind,
            file,
            line: line_start as u64,
            parent: parent.filter(|p| !p.is_empty()),
        }
    }

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

/// Split a lowercased name into alphanumeric tokens, matching the old Tantivy
/// default tokenizer (split on every non-alphanumeric char; camelCase is *not*
/// split). Returns empty when the name is already a single token equal to the
/// whole string — the whole-name match covers that case, so storing it again
/// would just double the per-query work.
fn name_tokens(name_lower: &str) -> Vec<String> {
    let toks: Vec<String> = name_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    match toks.as_slice() {
        [only] if only == name_lower => Vec::new(),
        _ => toks,
    }
}

/// Reusable scratch for bounded Levenshtein. Holds the two DP rows (and, for
/// the rare non-ASCII path, char buffers) so a whole fuzzy scan — millions of
/// distance calls — allocates them once instead of four heap Vecs per call,
/// which profiling showed dominated fuzzy (~34% of instructions in the
/// allocator, ~40% in the DP itself).
#[derive(Default)]
struct Lev {
    prev: Vec<usize>,
    cur: Vec<usize>,
    a_chars: Vec<char>,
    b_chars: Vec<char>,
}

impl Lev {
    /// Bounded Levenshtein distance between `a` and `b`, or `None` if it
    /// provably exceeds `max`. ASCII (the overwhelming case for code symbols)
    /// runs allocation-free over bytes; anything else falls back to a char DP.
    fn distance(&mut self, a: &str, b: &str, max: usize) -> Option<usize> {
        if a.is_ascii() && b.is_ascii() {
            self.dp(a.as_bytes(), b.as_bytes(), max)
        } else {
            // Cold path: materialize chars (reusing buffers) then DP over them.
            self.a_chars.clear();
            self.a_chars.extend(a.chars());
            self.b_chars.clear();
            self.b_chars.extend(b.chars());
            // Take the buffers out to satisfy the borrow checker, then DP.
            let a = std::mem::take(&mut self.a_chars);
            let b = std::mem::take(&mut self.b_chars);
            let out = self.dp(&a, &b, max);
            self.a_chars = a;
            self.b_chars = b;
            out
        }
    }

    /// Two-row bounded DP over any sequence of `Eq` items, reusing `self.{prev,
    /// cur}`. `prev[j]` = edit distance between `a[..i-1]` and `b[..j]`.
    fn dp<T: Eq>(&mut self, a: &[T], b: &[T], max: usize) -> Option<usize> {
        let (la, lb) = (a.len(), b.len());
        // Cheap length-gap prune FIRST, before touching the DP buffers — most
        // token comparisons fail here and now skip all buffer work.
        if la.abs_diff(lb) > max {
            return None;
        }
        let (prev, cur) = (&mut self.prev, &mut self.cur);
        prev.clear();
        prev.extend(0..=lb);
        cur.clear();
        cur.resize(lb + 1, 0);
        for i in 1..=la {
            cur[0] = i;
            let mut row_min = i;
            let ai = &a[i - 1];
            for j in 1..=lb {
                let cost = usize::from(*ai != b[j - 1]);
                cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
                row_min = row_min.min(cur[j]);
            }
            if row_min > max {
                return None;
            }
            std::mem::swap(prev, cur);
        }
        let d = prev[lb];
        (d <= max).then_some(d)
    }
}

/// One-shot bounded Levenshtein (allocates a fresh scratch). Test helper; the
/// hot paths build a [`Lev`] once and call `distance` in a loop instead.
#[cfg(test)]
fn bounded_levenshtein(a: &str, b: &str, max: usize) -> Option<usize> {
    Lev::default().distance(a, b, max)
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
    fn matches_tokens_within_compound_names() {
        // The old Tantivy index tokenized names, so prefix/fuzzy matched
        // *within* a snake_case / dotted name. Preserve that recall.
        let sym = |name: &str, kind| crate::types::Symbol {
            name: name.into(),
            kind,
            signature: None,
            parent: None,
            file: "a.rs".into(),
            line_start: 1,
            line_end: 1,
            visibility: None,
        };
        let fts = Fts::from_symbols(vec![
            sym("get_user_profile", SymbolKind::Function),
            sym("Authenticator", SymbolKind::Class),
        ]);
        // prefix on a mid-name token.
        let p: Vec<String> = fts
            .prefix("profile", 10)
            .unwrap()
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert!(p.iter().any(|n| n == "get_user_profile"), "prefix: {p:?}");
        // fuzzy on a typo'd token segment ("usr" → "user", distance 1).
        let f: Vec<String> = fts
            .fuzzy("usr", 10)
            .unwrap()
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert!(f.iter().any(|n| n == "get_user_profile"), "fuzzy: {f:?}");
    }

    #[test]
    fn fuzzy_bails_on_dense_matches() {
        // 5000 rows whose `user` token matches the query exactly. The scan must
        // bail early and still return exactly `limit` valid (distance-0) hits.
        let sym = |i: usize| crate::types::Symbol {
            name: format!("get_user_{i:05}"),
            kind: SymbolKind::Function,
            signature: None,
            parent: None,
            file: "a.rs".into(),
            line_start: i as u32,
            line_end: i as u32,
            visibility: None,
        };
        let fts = Fts::from_symbols((0..5000).map(sym));
        let hits = fts.fuzzy("user", 20).unwrap();
        assert_eq!(hits.len(), 20, "dense match should fill exactly the limit");
        assert!(hits.iter().all(|h| h.name.starts_with("get_user_")));
        // All are token-exact (distance 0 → score 1.0).
        assert!(hits.iter().all(|h| h.score == 1.0), "expected exact hits");
    }

    #[test]
    fn from_store_is_idempotent() {
        let (_dir, store, _fts) = fixture();
        let a = Fts::from_store(&store).unwrap().len();
        let b = Fts::from_store(&store).unwrap().len();
        assert_eq!(a, b);
    }

    /// Perf regression guard. Fuzzy/prefix are linear scans, so 4× the corpus
    /// should cost on the order of 4× the time — never the ~16× of an
    /// accidental O(N²) (e.g. a nested match or a per-row re-sort). We compare
    /// a small vs 4×-larger corpus with generous slack so this is robust on
    /// noisy/shared CI runners and only trips on a genuine algorithmic blow-up.
    #[test]
    fn fuzzy_prefix_scale_roughly_linearly() {
        use std::time::{Duration, Instant};

        fn synth(n: usize) -> Vec<crate::types::Symbol> {
            (0..n)
                .map(|i| crate::types::Symbol {
                    name: format!("sym_{i:05}"),
                    kind: SymbolKind::Function,
                    signature: None,
                    parent: None,
                    file: "synthetic.rs".into(),
                    line_start: i as u32,
                    line_end: i as u32,
                    visibility: None,
                })
                .collect()
        }

        // Best-of-N min timing of a batch, to damp scheduler noise.
        fn time_queries(n: usize) -> Duration {
            let fts = Fts::from_symbols(synth(n));
            let q = format!("sym_{:05}", n / 2);
            let prefix = &q[..q.len() - 1]; // matches ~10 rows (sort stays cheap)
            let run = || {
                for _ in 0..25 {
                    let _ = fts.fuzzy(&q, 20).unwrap();
                    let _ = fts.prefix(prefix, 20).unwrap();
                }
            };
            run(); // warm up
            let mut best = Duration::MAX;
            for _ in 0..4 {
                let t = Instant::now();
                run();
                best = best.min(t.elapsed());
            }
            best
        }

        let small = time_queries(5_000);
        let big = time_queries(20_000); // 4× the corpus

        // Linear would be ~4×; allow up to 12× for cache effects + scheduler
        // noise on shared CI runners. An O(N²) regression lands near 16× and
        // trips this. Skip the ratio when the larger run is already sub-2ms
        // (timer noise dominates and the absolute cost is a non-issue anyway).
        assert!(
            big < small * 12 || big < Duration::from_millis(2),
            "fuzzy/prefix scaling looks super-linear: 5k={small:?} 20k={big:?} \
             ({:.1}× — expected ~4× for a linear scan)",
            big.as_secs_f64() / small.as_secs_f64().max(f64::MIN_POSITIVE)
        );
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
