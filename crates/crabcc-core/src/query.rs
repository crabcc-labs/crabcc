use crate::pattern;
use crate::refs;
use crate::store::Store;
use crate::types::{Hit, Symbol};
use anyhow::Result;
use std::path::Path;

pub fn find_symbol(store: &Store, name: &str) -> Result<Vec<Symbol>> {
    store.find_by_name(name)
}

/// Find call sites of `name` across the indexed repo.
/// Iterates indexed files, prefilters textually with memchr, parses
/// candidates with ast-grep pattern `name($$$)`.
pub fn find_callers(store: &Store, root: &Path, name: &str) -> Result<Vec<Hit>> {
    run_per_file(store, root, name, |src, lang_str, file| {
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

/// Find every identifier reference to `name` across the indexed repo.
pub fn find_refs(store: &Store, root: &Path, name: &str) -> Result<Vec<Hit>> {
    run_per_file(store, root, name, |src, lang_str, file| {
        match refs::find_refs(src, lang_str, name) {
            Ok(mut hits) => {
                for h in &mut hits {
                    h.file = file.to_string();
                }
                hits
            }
            Err(_) => Vec::new(),
        }
    })
}

fn run_per_file<F>(store: &Store, root: &Path, name: &str, per_file: F) -> Result<Vec<Hit>>
where
    F: Fn(&str, &str, &str) -> Vec<Hit>,
{
    let needle = name.as_bytes();
    let mut out = Vec::new();
    for (rel_path, lang) in store.list_files()? {
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
        let hits = per_file(src, &lang, &rel_path);
        out.extend(hits);
    }
    Ok(out)
}

// TODO(crabcc/v1.1): callers from edges table once edges are populated.
pub fn callers_via_edges(_store: &Store, _name: &str) -> Result<Vec<()>> {
    Ok(Vec::new())
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
        write(&root.join("a.ts"),
              "export function greet(n: string){return n;}\nexport const x = greet(\"hi\");\n");
        write(&root.join("b.ts"),
              "import { greet } from './a';\ngreet('world');\nconst y = greet('again');\n");
        write(&root.join("c.rb"),
              "class User\nend\nUser.new\nUser.find(1)\n");
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
}
