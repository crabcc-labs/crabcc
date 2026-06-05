use crate::pattern;
use crate::refs;
use crate::store::{EdgeHit, Store};
use crate::types::{Hit, Symbol, SymbolKind};
use ahash::{AHashMap, HashSet};
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

pub mod blast_radius;
pub mod hot_symbols;
pub mod importers;
pub mod why;

/// How many entries each top-N list in `Output::Summary` returns.
/// Sized so the summary stays under a few KB even for hits-heavy queries —
/// agents can pivot to `Mode::Hits` if they need finer detail.
const DEFAULT_TOP_N: usize = 5;

pub fn find_symbol(store: &Store, name: &str) -> Result<Vec<Symbol>> {
    let r = store.find_by_name(name)?;
    tracing::debug!(target: "crabcc_core::query", name, hits = r.len(), "find_symbol");
    Ok(r)
}

/// Same as [`find_symbol`] but restricted to a set of repo-relative
/// file paths. Used by the `--since SHA` CLI/MCP filter.
pub fn find_symbol_in_files(
    store: &Store,
    name: &str,
    files: &HashSet<String>,
) -> Result<Vec<Symbol>> {
    let mut syms = store.find_by_name(name)?;
    syms.retain(|s| files.contains(&s.file));
    Ok(syms)
}

/// Mode for refs/callers queries — controls how much we materialize
/// before the agent ever sees the result. The flags are mutually exclusive
/// at the CLI level; precedence here is Count > Summary > FilesOnly > Hits(limit).
#[derive(Debug, Clone, Copy)]
pub enum Mode {
    /// Full hit list capped at `limit` (None = uncapped).
    Hits { limit: Option<usize> },
    /// Distinct file list, no line/col/snippet — capped at `limit`.
    FilesOnly { limit: Option<usize> },
    /// Per-file hit-count distribution: `{"by_file": {"path": N, ...}}`.
    /// Useful when an agent needs distribution-shape, not individual
    /// matches. Roughly 95% bytes saved vs raw hits, ~50% saved vs files-only.
    /// `limit` caps the number of files in the result (after sorting by
    /// path) — files dropped past the limit don't surface at all.
    Summary { limit: Option<usize> },
    /// Count of hits only — `{"count": N}`.
    Count,
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Hits { limit: None }
    }
}

/// One entry in `Output::Summary::top_files` — the file plus its hit count.
/// Sorted by `hits` descending in the output.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct FileHits {
    pub file: String,
    pub hits: usize,
}

/// One entry in `Output::Summary::top_symbols` — the enclosing symbol
/// (function, class, method, …) at each hit, plus how many hits landed
/// inside its line range. Sorted by `hits` descending.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SymbolHits {
    pub symbol: String,
    pub kind: SymbolKind,
    pub file: String,
    pub hits: usize,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Output {
    Hits(Vec<Hit>),
    Files {
        files: Vec<String>,
    },
    /// Distribution shape for agents that need shape, not individual hits.
    /// `by_file` is the full per-file count map (capped by `Mode::Summary::limit`).
    /// `top_files` and `top_symbols` are bounded leaderboards (size = `DEFAULT_TOP_N`)
    /// so the response stays small even on hits-heavy queries.
    Summary {
        by_file: BTreeMap<String, usize>,
        top_files: Vec<FileHits>,
        top_symbols: Vec<SymbolHits>,
    },
    Count {
        count: usize,
    },
}

impl Output {
    /// Approximate result count for tracking — total hits regardless of
    /// which output shape we picked. For `Summary`, returns the sum of
    /// per-file counts.
    pub fn count(&self) -> usize {
        match self {
            Output::Hits(h) => h.len(),
            Output::Files { files } => files.len(),
            Output::Summary { by_file, .. } => by_file.values().sum(),
            Output::Count { count } => *count,
        }
    }
}

/// Helper: aggregate raw hits into the `Summary` shape. Loads symbols
/// per file via the store (cached in a local map) and finds the
/// innermost enclosing symbol for each hit's line.
fn build_summary(store: &Store, hits: &[(String, u32)], limit: Option<usize>) -> Result<Output> {
    let mut by_file: BTreeMap<String, usize> = BTreeMap::new();
    for (file, _) in hits {
        *by_file.entry(file.clone()).or_insert(0) += 1;
    }

    // top_files: clone-then-sort the by_file map; truncate to top N.
    let mut top_files: Vec<FileHits> = by_file
        .iter()
        .map(|(f, n)| FileHits {
            file: f.clone(),
            hits: *n,
        })
        .collect();
    // Stable secondary sort by file path keeps ties deterministic — matters
    // for the fingerprint feature (#4) where output bytes must be stable.
    top_files.sort_by(|a, b| b.hits.cmp(&a.hits).then(a.file.cmp(&b.file)));
    top_files.truncate(DEFAULT_TOP_N);

    // top_symbols: for each hit, find the smallest enclosing symbol via
    // `Store::symbols_in_file`. Cache the per-file symbol vec so a hits-
    // heavy file pays the SQL once.
    // ahash is faster than std HashMap on small-key maps (hot inner
    // loop here — one entry per hit). DoS resistance unnecessary;
    // inputs are our own indexed data.
    let mut symbol_cache: AHashMap<String, Vec<Symbol>> = AHashMap::new();
    let mut tally: AHashMap<(String, String), (SymbolKind, usize)> = AHashMap::new();
    for (file, line) in hits {
        let symbols = match symbol_cache.get(file) {
            Some(v) => v,
            None => {
                let v = store.symbols_in_file(file).unwrap_or_default();
                symbol_cache.entry(file.clone()).or_insert(v)
            }
        };
        if let Some(sym) = enclosing_symbol(symbols, *line) {
            let entry = tally
                .entry((sym.name.clone(), file.clone()))
                .or_insert((sym.kind, 0));
            entry.1 += 1;
        }
    }
    let mut top_symbols: Vec<SymbolHits> = tally
        .into_iter()
        .map(|((name, file), (kind, hits))| SymbolHits {
            symbol: name,
            kind,
            file,
            hits,
        })
        .collect();
    top_symbols.sort_by(|a, b| {
        b.hits
            .cmp(&a.hits)
            .then(a.file.cmp(&b.file))
            .then(a.symbol.cmp(&b.symbol))
    });
    top_symbols.truncate(DEFAULT_TOP_N);

    if let Some(l) = limit {
        while by_file.len() > l {
            by_file.pop_last();
        }
    }

    Ok(Output::Summary {
        by_file,
        top_files,
        top_symbols,
    })
}

/// Find the innermost (smallest line range) symbol containing `line`.
/// Returns `None` if no symbol's `[line_start, line_end]` covers it —
/// e.g., top-level statements that aren't inside a function/class.
fn enclosing_symbol(symbols: &[Symbol], line: u32) -> Option<&Symbol> {
    symbols
        .iter()
        .filter(|s| s.line_start <= line && line <= s.line_end)
        .min_by_key(|s| s.line_end.saturating_sub(s.line_start))
}

/// Find call sites of `name` across the indexed repo.
pub fn find_callers(store: &Store, root: &Path, name: &str) -> Result<Vec<Hit>> {
    match query_callers(store, root, name, Mode::default(), None)? {
        Output::Hits(h) => Ok(h),
        _ => unreachable!("default mode is Hits"),
    }
}

/// Find every identifier reference to `name` across the indexed repo.
pub fn find_refs(store: &Store, root: &Path, name: &str) -> Result<Vec<Hit>> {
    match query_refs(store, root, name, Mode::default(), None)? {
        Output::Hits(h) => Ok(h),
        _ => unreachable!("default mode is Hits"),
    }
}

/// Find call sites of `name`. When `file_filter` is `Some`, only files
/// in that set are considered — used by the `--since SHA` flag (which
/// resolves the SHA to a changed-files set via `gitdiff::changed_files_since`).
pub fn query_callers(
    store: &Store,
    root: &Path,
    name: &str,
    mode: Mode,
    file_filter: Option<&HashSet<String>>,
) -> Result<Output> {
    let started = std::time::Instant::now();
    // Fast path: edges populated by `crabcc index` v2.0+. One SQL query
    // replaces N tree-sitter walks. Falls back to the ast-grep walker for
    // partially-populated indexes (v1.0.0 upgrade where edges_populated='0').
    if edges_ready(store)? {
        let r = callers_via_edges(store, root, name, mode, file_filter)?;
        tracing::debug!(
            target: "crabcc_core::query",
            name,
            count = r.count(),
            path = "edges-fast",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "query_callers"
        );
        return Ok(r);
    }
    run(
        store,
        root,
        name,
        mode,
        file_filter,
        |src, lang_str, file| {
            let Some(lang) = pattern::lang_for(lang_str) else {
                return Vec::new();
            };
            let mut hits = pattern::find_callers(src, lang, name);
            for h in &mut hits {
                h.file = file.to_string();
            }
            hits
        },
    )
}

fn edges_ready(store: &Store) -> Result<bool> {
    Ok(store.meta_get("edges_populated")?.as_deref() == Some("1"))
}

/// Pure-SQL caller resolution over the `edges` table. Snippets for the
/// `Hits` shape are read on demand from disk — we group by file so each
/// file is read at most once even when many call sites land in the same one.
pub fn callers_via_edges(
    store: &Store,
    root: &Path,
    name: &str,
    mode: Mode,
    file_filter: Option<&HashSet<String>>,
) -> Result<Output> {
    // The edges table stores only bare method names (e.g. `open`, not
    // `Store::open`), so strip the qualifier before the SQL lookup.
    let name = bare_name(name);
    if !is_safe_identifier(name) {
        return Ok(empty_for(mode));
    }
    let edge_hits = store.callers_of(name)?;
    edge_hits_to_output(store, root, mode, file_filter, edge_hits)
}

/// Edge-driven `lookup refs` fast path: same shape as `callers_via_edges`
/// but pulls every reference-like kind (`call` ∪ `ref`). The CLI surface
/// uses this when the SQL edge index is populated; mirrors the LSP
/// `references` handler.
pub fn refs_via_edges(
    store: &Store,
    root: &Path,
    name: &str,
    mode: Mode,
    file_filter: Option<&HashSet<String>>,
) -> Result<Output> {
    let name = bare_name(name);
    if !is_safe_identifier(name) {
        return Ok(empty_for(mode));
    }
    let edge_hits = store.refs_of(name)?;
    edge_hits_to_output(store, root, mode, file_filter, edge_hits)
}

/// Shared post-processing for `callers_via_edges` / `refs_via_edges` —
/// applies the file filter then dispatches on `Mode`. Loads snippets from
/// disk grouped per file for the `Hits` shape so each file is read at most
/// once even when many edges land in it.
fn edge_hits_to_output(
    store: &Store,
    root: &Path,
    mode: Mode,
    file_filter: Option<&HashSet<String>>,
    edge_hits: Vec<EdgeHit>,
) -> Result<Output> {
    let edge_hits: Vec<_> = match file_filter {
        Some(set) => edge_hits
            .into_iter()
            .filter(|h| set.contains(&h.file))
            .collect(),
        None => edge_hits,
    };

    match mode {
        Mode::Count => Ok(Output::Count {
            count: edge_hits.len(),
        }),
        Mode::FilesOnly { limit } => {
            let mut seen: HashSet<&str> = HashSet::default();
            let mut files: Vec<String> = Vec::new();
            for h in &edge_hits {
                if seen.insert(h.file.as_str()) {
                    files.push(h.file.clone());
                    if let Some(l) = limit {
                        if files.len() >= l {
                            break;
                        }
                    }
                }
            }
            Ok(Output::Files { files })
        }
        Mode::Summary { limit } => {
            let pairs: Vec<(String, u32)> =
                edge_hits.iter().map(|h| (h.file.clone(), h.line)).collect();
            build_summary(store, &pairs, limit)
        }
        Mode::Hits { limit } => {
            let mut grouped: BTreeMap<String, Vec<u32>> = BTreeMap::new();
            for h in edge_hits {
                grouped.entry(h.file).or_default().push(h.line);
            }
            let mut hits: Vec<Hit> = Vec::new();
            for (file, mut lines) in grouped {
                if let Some(l) = limit {
                    if hits.len() >= l {
                        break;
                    }
                }
                lines.sort_unstable();
                let full = root.join(&file);
                let text = match std::fs::read_to_string(&full) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let by_line: Vec<&str> = text.lines().collect();
                for line in lines {
                    if let Some(l) = limit {
                        if hits.len() >= l {
                            break;
                        }
                    }
                    let idx = (line as usize).saturating_sub(1);
                    let raw = by_line.get(idx).copied().unwrap_or_default();
                    hits.push(Hit {
                        file: file.clone(),
                        line,
                        col: 1,
                        snippet: compact_snippet(raw),
                    });
                }
            }
            Ok(Output::Hits(hits))
        }
    }
}

fn empty_for(mode: Mode) -> Output {
    match mode {
        Mode::Count => Output::Count { count: 0 },
        Mode::FilesOnly { .. } => Output::Files { files: Vec::new() },
        Mode::Summary { .. } => Output::Summary {
            by_file: BTreeMap::new(),
            top_files: Vec::new(),
            top_symbols: Vec::new(),
        },
        Mode::Hits { .. } => Output::Hits(Vec::new()),
    }
}

fn is_safe_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

/// Strip a Rust path qualifier — `Store::open` → `open`. The edges table
/// stores bare method names, so we drop the type qualifier before the SQL
/// lookup. Lossy: `Foo::open` and `Bar::open` collapse onto `open` and the
/// union of their call sites is returned.
fn bare_name(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

fn compact_snippet(s: &str) -> String {
    let one_line = s.split_whitespace().fold(String::new(), |mut acc, w| {
        if !acc.is_empty() {
            acc.push(' ');
        }
        acc.push_str(w);
        acc
    });
    // Truncate at a char boundary (the 80th char's byte index), not a fixed
    // byte offset — `&one_line[..80]` panics if byte 80 splits a multibyte char
    // (any non-ASCII source line > 80 bytes).
    match one_line.char_indices().nth(80) {
        Some((idx, _)) => format!("{}…", &one_line[..idx]),
        None => one_line,
    }
}

pub fn query_refs(
    store: &Store,
    root: &Path,
    name: &str,
    mode: Mode,
    file_filter: Option<&HashSet<String>>,
) -> Result<Output> {
    let started = std::time::Instant::now();
    let r = run(
        store,
        root,
        name,
        mode,
        file_filter,
        |src, lang_str, file| match refs::find_refs(src, lang_str, name) {
            Ok(mut hits) => {
                for h in &mut hits {
                    h.file = file.to_string();
                }
                hits
            }
            Err(_) => Vec::new(),
        },
    )?;
    // refs::find_refs only covers JS/TS/Ruby. For everything else (Rust,
    // Python, Go, Swift, Bash, Java) it errors and `r` is empty. Fall back
    // to the broader edge-based index (call ∪ ref) so type references
    // surface alongside call sites — mirrors the LSP `references` handler.
    // When edges aren't populated (legacy v1 indexes) drop to the walker
    // path inside `query_callers` so the surface stays non-empty.
    let r = if r.count() == 0 {
        if edges_ready(store)? {
            refs_via_edges(store, root, name, mode, file_filter)?
        } else {
            query_callers(store, root, name, mode, file_filter)?
        }
    } else {
        r
    };
    tracing::debug!(
        target: "crabcc_core::query",
        name,
        count = r.count(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "query_refs"
    );
    Ok(r)
}

fn run<F>(
    store: &Store,
    root: &Path,
    name: &str,
    mode: Mode,
    file_filter: Option<&HashSet<String>>,
    per_file: F,
) -> Result<Output>
where
    F: Fn(&str, &str, &str) -> Vec<Hit>,
{
    let needle = name.as_bytes();
    let mut hits: Vec<Hit> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut seen_files: HashSet<String> = HashSet::default();
    let mut summary_hits: Vec<(String, u32)> = Vec::new();
    let mut count: usize = 0;

    for (rel_path, lang) in store.list_files()? {
        if early_stop(&mode, hits.len(), files.len()) {
            break;
        }
        // `--since SHA` filter — skip files outside the changed-files set
        // before any IO. Cheaper than reading the file then discarding.
        if let Some(set) = file_filter {
            if !set.contains(&rel_path) {
                continue;
            }
        }

        let full = root.join(&rel_path);
        let bytes = match std::fs::read(&full) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if memchr::memmem::find(&bytes, needle).is_none() {
            continue;
        }
        let src = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };

        match mode {
            Mode::Count => {
                let n = per_file(src, &lang, &rel_path).len();
                count += n;
            }
            Mode::FilesOnly { limit } => {
                let n = per_file(src, &lang, &rel_path).len();
                if n > 0 && seen_files.insert(rel_path.clone()) {
                    files.push(rel_path);
                    if let Some(l) = limit {
                        if files.len() >= l {
                            break;
                        }
                    }
                }
            }
            Mode::Summary { .. } => {
                let new_hits = per_file(src, &lang, &rel_path);
                for h in &new_hits {
                    summary_hits.push((rel_path.clone(), h.line));
                }
                // Note: `limit` is applied after the walk completes so
                // the result stays sorted-by-path and stable. Stopping
                // mid-walk would leak walk-order into the response.
            }
            Mode::Hits { limit } => {
                let mut new_hits = per_file(src, &lang, &rel_path);
                if let Some(l) = limit {
                    let room = l.saturating_sub(hits.len());
                    if new_hits.len() > room {
                        new_hits.truncate(room);
                    }
                }
                hits.extend(new_hits);
                if let Some(l) = limit {
                    if hits.len() >= l {
                        break;
                    }
                }
            }
        }
    }

    match mode {
        Mode::Hits { .. } => Ok(Output::Hits(hits)),
        Mode::FilesOnly { .. } => Ok(Output::Files { files }),
        Mode::Summary { limit } => build_summary(store, &summary_hits, limit),
        Mode::Count => Ok(Output::Count { count }),
    }
}

fn early_stop(mode: &Mode, hits_len: usize, files_len: usize) -> bool {
    match mode {
        Mode::Hits { limit: Some(l) } => hits_len >= *l,
        Mode::FilesOnly { limit: Some(l) } => files_len >= *l,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{build_index, full_index};

    #[test]
    fn compact_snippet_truncates_multibyte_without_panicking() {
        // 3-byte chars so byte 80 lands mid-char — the old `&one_line[..80]`
        // would panic. Must truncate on a char boundary.
        let out = compact_snippet(&"€".repeat(100));
        assert!(out.ends_with('…'), "expected ellipsis: {out:?}");
        assert_eq!(out.chars().count(), 81, "80 chars + ellipsis: {out:?}");
    }

    fn write(p: &Path, body: &str) {
        std::fs::write(p, body).unwrap();
    }

    fn fixture_repo() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("a.ts"),
            "export function greet(n: string){return n;}\nexport const x = greet(\"hi\");\n",
        );
        write(
            &root.join("b.ts"),
            "import { greet } from './a';\ngreet('world');\nconst y = greet('again');\n",
        );
        write(
            &root.join("c.rb"),
            "class User\nend\nUser.new\nUser.find(1)\n",
        );
        let store = Store::open(&root.join("idx.db")).unwrap();
        build_index(root, &store).unwrap();
        (dir, store)
    }

    #[test]
    fn callers_finds_typescript_calls() {
        let (dir, store) = fixture_repo();
        let hits = find_callers(&store, dir.path(), "greet").unwrap();
        // 3 call sites: a.ts:2, b.ts:2, b.ts:3
        assert!(hits.len() >= 3, "got: {hits:?}");
        assert!(hits.iter().any(|h| h.file == "b.ts"));
    }

    #[test]
    fn refs_finds_typescript_and_ruby_idents() {
        let (dir, store) = fixture_repo();
        let ts_hits = find_refs(&store, dir.path(), "greet").unwrap();
        // Definition + import + 2 calls + 1 export → at least 4.
        assert!(ts_hits.len() >= 4, "ts hits: {ts_hits:?}");

        let ruby_hits = find_refs(&store, dir.path(), "User").unwrap();
        assert!(ruby_hits.len() >= 3, "ruby hits: {ruby_hits:?}");
    }

    #[test]
    fn unknown_name_returns_empty() {
        let (dir, store) = fixture_repo();
        let hits = find_callers(&store, dir.path(), "nope_definitely_not").unwrap();
        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn invalid_identifier_safe() {
        let (dir, store) = fixture_repo();
        let hits = find_callers(&store, dir.path(), "ab cd").unwrap();
        // memchr might match the substring "ab cd" but pattern compile rejects.
        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn refs_count_mode_returns_total() {
        let (dir, store) = fixture_repo();
        let out = query_refs(&store, dir.path(), "greet", Mode::Count, None).unwrap();
        match out {
            Output::Count { count } => assert!(count >= 4, "count: {count}"),
            _ => panic!("expected Count output"),
        }
    }

    #[test]
    fn refs_files_only_dedupes_per_file() {
        let (dir, store) = fixture_repo();
        let out = query_refs(
            &store,
            dir.path(),
            "greet",
            Mode::FilesOnly { limit: None },
            None,
        )
        .unwrap();
        match out {
            Output::Files { files } => {
                assert!(files.contains(&"a.ts".to_string()));
                assert!(files.contains(&"b.ts".to_string()));
                assert_eq!(files.len(), 2, "expected 2 distinct files, got: {files:?}");
            }
            _ => panic!("expected Files output"),
        }
    }

    #[test]
    fn refs_summary_mode_returns_per_file_distribution_and_top_files() {
        // The walker path — refs uses `run`, not the edges fast path.
        // `greet` lives in a.ts (definition + export) and is referenced
        // again in b.ts (import + 2 calls). The summary must surface
        // both files with per-file counts AND a top_files leaderboard
        // sorted by hits descending.
        let (dir, store) = fixture_repo();
        let out = query_refs(
            &store,
            dir.path(),
            "greet",
            Mode::Summary { limit: None },
            None,
        )
        .unwrap();
        match out {
            Output::Summary {
                by_file,
                top_files,
                top_symbols,
            } => {
                assert!(by_file.contains_key("a.ts"));
                assert!(by_file.contains_key("b.ts"));
                assert!(by_file["a.ts"] >= 1);
                assert!(by_file["b.ts"] >= 1);

                // top_files must be sorted hits-desc and contain both
                // files (no truncation since corpus has 2 hit-bearing
                // files and DEFAULT_TOP_N is 5).
                assert_eq!(top_files.len(), 2, "got: {top_files:?}");
                assert!(
                    top_files[0].hits >= top_files[1].hits,
                    "top_files not desc: {top_files:?}"
                );

                // top_symbols depends on the indexer extracting `greet`
                // as a function symbol in a.ts. The fixture's a.ts
                // defines `greet` so we should see at least one entry.
                assert!(
                    !top_symbols.is_empty(),
                    "expected non-empty top_symbols, got: {top_symbols:?}"
                );
            }
            _ => panic!("expected Summary output, got {out:?}"),
        }
    }

    #[test]
    fn refs_summary_limit_caps_by_file_only() {
        // `limit` on Mode::Summary caps `by_file`, not the leaderboards.
        // With limit=1 only the first file (sorted by path) survives.
        let (dir, store) = fixture_repo();
        let out = query_refs(
            &store,
            dir.path(),
            "greet",
            Mode::Summary { limit: Some(1) },
            None,
        )
        .unwrap();
        match out {
            Output::Summary { by_file, .. } => {
                assert_eq!(by_file.len(), 1, "limit=1 should keep one file");
                assert!(by_file.contains_key("a.ts"));
                assert!(!by_file.contains_key("b.ts"));
            }
            _ => panic!("expected Summary output"),
        }
    }

    #[test]
    fn callers_summary_mode_includes_enclosing_symbol() {
        // Callers takes the edges fast path when `edges_populated='1'`,
        // otherwise the legacy walker. `fixture_repo` calls `build_index`,
        // not `full_index`, so this exercises the walker path. Both
        // paths must produce equivalent Summary output for the contract
        // to hold; specifically — every call site of `greet` in b.ts
        // sits inside the function-level scope of that file.
        let (dir, store) = fixture_repo();
        let out = query_callers(
            &store,
            dir.path(),
            "greet",
            Mode::Summary { limit: None },
            None,
        )
        .unwrap();
        match out {
            Output::Summary {
                by_file, top_files, ..
            } => {
                let total: usize = by_file.values().sum();
                assert!(total >= 3, "expected ≥3 calls, got {total}");
                assert!(by_file.contains_key("b.ts"));
                assert!(top_files.iter().any(|f| f.file == "b.ts"));
            }
            _ => panic!("expected Summary output"),
        }
    }

    #[test]
    fn unknown_name_summary_is_empty_with_empty_leaderboards() {
        let (dir, store) = fixture_repo();
        let out = query_refs(
            &store,
            dir.path(),
            "absolutely_not_in_fixture",
            Mode::Summary { limit: None },
            None,
        )
        .unwrap();
        match out {
            Output::Summary {
                by_file,
                top_files,
                top_symbols,
            } => {
                assert!(by_file.is_empty());
                assert!(top_files.is_empty());
                assert!(top_symbols.is_empty());
            }
            _ => panic!("expected Summary output"),
        }
    }

    #[test]
    fn find_symbol_in_files_filters_by_path() {
        // Multiple files defining `greet`; restrict to one.
        let (_dir, store) = fixture_repo();
        let only_a: HashSet<String> = ["a.ts".to_string()].into_iter().collect();
        let syms = find_symbol_in_files(&store, "greet", &only_a).unwrap();
        assert!(!syms.is_empty(), "expected hit in a.ts");
        for s in &syms {
            assert_eq!(s.file, "a.ts");
        }

        // Empty filter set drops everything — used by `--since` when no
        // files have changed in the window.
        let empty: HashSet<String> = HashSet::default();
        let syms = find_symbol_in_files(&store, "greet", &empty).unwrap();
        assert!(syms.is_empty());
    }

    #[test]
    fn query_refs_with_file_filter_skips_other_files() {
        // `greet` lives in both a.ts and b.ts. With a filter set
        // containing only b.ts, the response must only contain b.ts hits.
        let (dir, store) = fixture_repo();
        let only_b: HashSet<String> = ["b.ts".to_string()].into_iter().collect();
        let out = query_refs(
            &store,
            dir.path(),
            "greet",
            Mode::Hits { limit: None },
            Some(&only_b),
        )
        .unwrap();
        match out {
            Output::Hits(hs) => {
                assert!(!hs.is_empty(), "expected at least one hit in b.ts");
                for h in &hs {
                    assert_eq!(h.file, "b.ts", "unexpected file: {h:?}");
                }
            }
            _ => panic!("expected Hits output"),
        }
    }

    #[test]
    fn query_callers_with_empty_filter_returns_zero_hits() {
        // Empty filter set — equivalent to `--since` against a SHA where
        // none of the indexed files have changed. Both code paths
        // (edges + walker) must return zero hits without erroring.
        let (dir, store) = fixture_repo();
        let empty: HashSet<String> = HashSet::default();
        let out = query_callers(&store, dir.path(), "greet", Mode::Count, Some(&empty)).unwrap();
        match out {
            Output::Count { count } => assert_eq!(count, 0),
            _ => panic!("expected Count output"),
        }
    }

    #[test]
    fn enclosing_symbol_picks_innermost() {
        use crate::types::SymbolKind;
        let outer = Symbol {
            name: "outer".into(),
            kind: SymbolKind::Class,
            signature: None,
            parent: None,
            file: "x.ts".into(),
            line_start: 1,
            line_end: 50,
            visibility: None,
        };
        let inner = Symbol {
            name: "inner".into(),
            kind: SymbolKind::Method,
            signature: None,
            parent: Some("outer".into()),
            file: "x.ts".into(),
            line_start: 10,
            line_end: 20,
            visibility: None,
        };
        let symbols = vec![outer, inner];
        // Line 15 sits inside both — `inner` has the smaller range and wins.
        let chosen = enclosing_symbol(&symbols, 15).unwrap();
        assert_eq!(chosen.name, "inner");
        // Line 5 sits only inside `outer`.
        let chosen = enclosing_symbol(&symbols, 5).unwrap();
        assert_eq!(chosen.name, "outer");
        // Line 100 sits in neither.
        assert!(enclosing_symbol(&symbols, 100).is_none());
    }

    #[test]
    fn callers_limit_caps_results() {
        let (dir, store) = fixture_repo();
        let out = query_callers(
            &store,
            dir.path(),
            "greet",
            Mode::Hits { limit: Some(1) },
            None,
        )
        .unwrap();
        match out {
            Output::Hits(h) => assert_eq!(h.len(), 1),
            _ => panic!("expected Hits output"),
        }
    }

    #[test]
    fn callers_count_mode_aggregates() {
        let (dir, store) = fixture_repo();
        let out = query_callers(&store, dir.path(), "greet", Mode::Count, None).unwrap();
        match out {
            Output::Count { count } => assert!(count >= 2, "got: {count}"),
            _ => panic!("expected Count output"),
        }
    }

    #[test]
    fn callers_files_only_dedupes() {
        let (dir, store) = fixture_repo();
        let out = query_callers(
            &store,
            dir.path(),
            "greet",
            Mode::FilesOnly { limit: None },
            None,
        )
        .unwrap();
        match out {
            Output::Files { files } => {
                // greet has callers in both a.ts and b.ts.
                assert!(files.contains(&"a.ts".to_string()) || files.contains(&"b.ts".to_string()));
                // No duplicates per file.
                let mut seen: HashSet<String> = HashSet::default();
                for f in &files {
                    assert!(seen.insert(f.clone()), "duplicate file: {f}");
                }
            }
            _ => panic!("expected Files output"),
        }
    }

    #[test]
    fn files_only_limit_truncates() {
        let (dir, store) = fixture_repo();
        let out = query_refs(
            &store,
            dir.path(),
            "greet",
            Mode::FilesOnly { limit: Some(1) },
            None,
        )
        .unwrap();
        match out {
            Output::Files { files } => assert_eq!(files.len(), 1),
            _ => panic!("expected Files output"),
        }
    }

    #[test]
    fn output_count_helper_matches_payload() {
        let (dir, store) = fixture_repo();
        let h = query_refs(&store, dir.path(), "greet", Mode::default(), None).unwrap();
        let f = query_refs(
            &store,
            dir.path(),
            "greet",
            Mode::FilesOnly { limit: None },
            None,
        )
        .unwrap();
        let c = query_refs(&store, dir.path(), "greet", Mode::Count, None).unwrap();
        match (&h, &f, &c) {
            (Output::Hits(hits), Output::Files { files }, Output::Count { count }) => {
                assert_eq!(h.count(), hits.len());
                assert_eq!(f.count(), files.len());
                assert_eq!(c.count(), *count);
            }
            _ => panic!("unexpected output combo"),
        }
    }

    #[test]
    fn limit_zero_treated_as_unlimited_via_default() {
        // The CLI maps `--limit 0` → None; in core that means "no cap". Verify.
        let (dir, store) = fixture_repo();
        let limited = query_refs(
            &store,
            dir.path(),
            "greet",
            Mode::Hits { limit: None },
            None,
        )
        .unwrap();
        let default = query_refs(&store, dir.path(), "greet", Mode::default(), None).unwrap();
        assert_eq!(limited.count(), default.count());
    }

    // ---- SQL-edges caller path ----

    #[test]
    fn callers_via_edges_finds_typescript_calls() {
        let (dir, store) = fixture_repo();
        // After build_index, edges_populated='1' — query_callers takes SQL path.
        let out = query_callers(&store, dir.path(), "greet", Mode::default(), None).unwrap();
        match out {
            Output::Hits(hits) => {
                assert!(hits.len() >= 2, "expected ≥2 greet callers: {hits:?}");
                assert!(hits.iter().any(|h| h.file == "b.ts"));
            }
            _ => panic!("expected Hits"),
        }
    }

    #[test]
    fn callers_via_edges_count_matches_legacy() {
        // Build the same fixture, run both paths, expect parity.
        let (dir, store) = fixture_repo();
        let sql = match query_callers(&store, dir.path(), "greet", Mode::Count, None).unwrap() {
            Output::Count { count } => count,
            _ => unreachable!(),
        };
        // Force a legacy run by clearing the gate flag.
        store.meta_set("edges_populated", "0").unwrap();
        let ast = match query_callers(&store, dir.path(), "greet", Mode::Count, None).unwrap() {
            Output::Count { count } => count,
            _ => unreachable!(),
        };
        // Restore the flag so other tests aren't affected.
        store.meta_set("edges_populated", "1").unwrap();
        // SQL path should agree with the ast-grep walker on first-party code.
        // We allow ±1 because tree-sitter and ast-grep may differ on edge
        // cases like template-string method calls.
        assert!((sql as i64 - ast as i64).abs() <= 1, "sql={sql} ast={ast}");
    }

    #[test]
    fn callers_via_edges_invalid_name_is_empty() {
        let (dir, store) = fixture_repo();
        let out = query_callers(&store, dir.path(), "ab cd", Mode::default(), None).unwrap();
        match out {
            Output::Hits(hits) => assert!(hits.is_empty()),
            _ => panic!("expected Hits"),
        }
    }

    #[test]
    fn callers_via_edges_files_only_dedupes() {
        let (dir, store) = fixture_repo();
        let out = query_callers(
            &store,
            dir.path(),
            "greet",
            Mode::FilesOnly { limit: None },
            None,
        )
        .unwrap();
        match out {
            Output::Files { files } => {
                let set: std::collections::HashSet<&String> = files.iter().collect();
                assert_eq!(set.len(), files.len(), "duplicates: {files:?}");
            }
            _ => panic!("expected Files"),
        }
    }

    fn fixture_rust() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("lib.rs"),
            "pub struct Store;\n\
             impl Store {\n    pub fn open() -> Store { Store }\n}\n\
             pub fn run() {\n    let _ = Store::open();\n}\n",
        );
        let store = Store::open(&root.join("idx.db")).unwrap();
        // full_index flips edges_populated='1' — required to exercise the
        // callers_via_edges fast path that real `crabcc index` runs use.
        full_index(root, &store).unwrap();
        (dir, store)
    }

    #[test]
    fn bare_name_strips_rust_qualifier() {
        assert_eq!(bare_name("Store::open"), "open");
        assert_eq!(bare_name("a::b::c"), "c");
        assert_eq!(bare_name("open"), "open");
        assert_eq!(bare_name(""), "");
    }

    #[test]
    fn refs_falls_back_to_callers_for_rust() {
        // Regression: refs::find_refs only supports JS/TS/Ruby, so Rust
        // queries used to return [] silently. query_refs now falls back
        // to the edge-based caller index when the walker is empty.
        let (dir, store) = fixture_rust();
        let hits = find_refs(&store, dir.path(), "open").unwrap();
        assert!(
            !hits.is_empty(),
            "expected Rust callers via fallback, got: {hits:?}"
        );
    }

    #[test]
    fn callers_resolves_qualified_rust_names() {
        // Regression: `Store::open` was rejected by is_safe_identifier;
        // the edges table only stores the bare method name, so strip the
        // qualifier and query the tail.
        let (dir, store) = fixture_rust();
        let hits = find_callers(&store, dir.path(), "Store::open").unwrap();
        assert!(
            !hits.is_empty(),
            "Store::open should resolve to bare 'open' call site, got: {hits:?}"
        );
    }

    #[test]
    fn refs_finds_rust_struct_usages() {
        // End-to-end: extractor emits kind=ref edges for `type_identifier`
        // uses, refs_via_edges queries both `call` and `ref` kinds, so
        // structs (which are never *called*) now surface their usages.
        let (dir, store) = fixture_rust();
        let hits = find_refs(&store, dir.path(), "Store").unwrap();
        assert!(
            !hits.is_empty(),
            "Store struct should have ref hits from impl/return-type, got: {hits:?}"
        );
    }
}
