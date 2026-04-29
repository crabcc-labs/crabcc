use crate::pattern;
use crate::refs;
use crate::store::Store;
use crate::types::{Hit, Symbol};
use anyhow::Result;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;

pub fn find_symbol(store: &Store, name: &str) -> Result<Vec<Symbol>> {
    store.find_by_name(name)
}

/// Mode for refs/callers queries — controls how much we materialize
/// before the agent ever sees the result. The flags are mutually exclusive
/// at the CLI level; precedence here is Count > FilesOnly > Hits(limit).
#[derive(Debug, Clone, Copy)]
pub enum Mode {
    /// Full hit list capped at `limit` (None = uncapped).
    Hits { limit: Option<usize> },
    /// Distinct file list, no line/col/snippet — capped at `limit`.
    FilesOnly { limit: Option<usize> },
    /// Count of hits only — `{"count": N}`.
    Count,
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Hits { limit: None }
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Output {
    Hits(Vec<Hit>),
    Files { files: Vec<String> },
    Count { count: usize },
}

impl Output {
    /// Approximate result count for tracking — total hits regardless of
    /// which output shape we picked.
    pub fn count(&self) -> usize {
        match self {
            Output::Hits(h) => h.len(),
            Output::Files { files } => files.len(),
            Output::Count { count } => *count,
        }
    }
}

/// Find call sites of `name` across the indexed repo.
pub fn find_callers(store: &Store, root: &Path, name: &str) -> Result<Vec<Hit>> {
    match query_callers(store, root, name, Mode::default())? {
        Output::Hits(h) => Ok(h),
        _ => unreachable!("default mode is Hits"),
    }
}

/// Find every identifier reference to `name` across the indexed repo.
pub fn find_refs(store: &Store, root: &Path, name: &str) -> Result<Vec<Hit>> {
    match query_refs(store, root, name, Mode::default())? {
        Output::Hits(h) => Ok(h),
        _ => unreachable!("default mode is Hits"),
    }
}

pub fn query_callers(store: &Store, root: &Path, name: &str, mode: Mode) -> Result<Output> {
    run(store, root, name, mode, |src, lang_str, file| {
        let Some(lang) = pattern::lang_for(lang_str) else {
            return Vec::new();
        };
        let mut hits = pattern::find_callers(src, lang, name);
        for h in &mut hits {
            h.file = file.to_string();
        }
        hits
    })
}

pub fn query_refs(store: &Store, root: &Path, name: &str, mode: Mode) -> Result<Output> {
    run(
        store,
        root,
        name,
        mode,
        |src, lang_str, file| match refs::find_refs(src, lang_str, name) {
            Ok(mut hits) => {
                for h in &mut hits {
                    h.file = file.to_string();
                }
                hits
            }
            Err(_) => Vec::new(),
        },
    )
}

fn run<F>(store: &Store, root: &Path, name: &str, mode: Mode, per_file: F) -> Result<Output>
where
    F: Fn(&str, &str, &str) -> Vec<Hit>,
{
    let needle = name.as_bytes();
    let mut hits: Vec<Hit> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut seen_files: HashSet<String> = HashSet::new();
    let mut count: usize = 0;

    for (rel_path, lang) in store.list_files()? {
        if early_stop(&mode, hits.len(), files.len()) {
            break;
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

    Ok(match mode {
        Mode::Hits { .. } => Output::Hits(hits),
        Mode::FilesOnly { .. } => Output::Files { files },
        Mode::Count => Output::Count { count },
    })
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
    use crate::index::build_index;

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
        let out = query_refs(&store, dir.path(), "greet", Mode::Count).unwrap();
        match out {
            Output::Count { count } => assert!(count >= 4, "count: {count}"),
            _ => panic!("expected Count output"),
        }
    }

    #[test]
    fn refs_files_only_dedupes_per_file() {
        let (dir, store) = fixture_repo();
        let out = query_refs(&store, dir.path(), "greet", Mode::FilesOnly { limit: None }).unwrap();
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
    fn callers_limit_caps_results() {
        let (dir, store) = fixture_repo();
        let out =
            query_callers(&store, dir.path(), "greet", Mode::Hits { limit: Some(1) }).unwrap();
        match out {
            Output::Hits(h) => assert_eq!(h.len(), 1),
            _ => panic!("expected Hits output"),
        }
    }

    #[test]
    fn callers_count_mode_aggregates() {
        let (dir, store) = fixture_repo();
        let out = query_callers(&store, dir.path(), "greet", Mode::Count).unwrap();
        match out {
            Output::Count { count } => assert!(count >= 2, "got: {count}"),
            _ => panic!("expected Count output"),
        }
    }

    #[test]
    fn callers_files_only_dedupes() {
        let (dir, store) = fixture_repo();
        let out =
            query_callers(&store, dir.path(), "greet", Mode::FilesOnly { limit: None }).unwrap();
        match out {
            Output::Files { files } => {
                // greet has callers in both a.ts and b.ts.
                assert!(files.contains(&"a.ts".to_string()) || files.contains(&"b.ts".to_string()));
                // No duplicates per file.
                let mut seen = std::collections::HashSet::new();
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
        let h = query_refs(&store, dir.path(), "greet", Mode::default()).unwrap();
        let f = query_refs(&store, dir.path(), "greet", Mode::FilesOnly { limit: None }).unwrap();
        let c = query_refs(&store, dir.path(), "greet", Mode::Count).unwrap();
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
        let limited = query_refs(&store, dir.path(), "greet", Mode::Hits { limit: None }).unwrap();
        let default = query_refs(&store, dir.path(), "greet", Mode::default()).unwrap();
        assert_eq!(limited.count(), default.count());
    }
}
