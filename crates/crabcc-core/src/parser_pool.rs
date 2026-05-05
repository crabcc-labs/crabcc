//! Thread-local tree-sitter `Parser` pool.
//!
//! tree-sitter's docs explicitly recommend reusing a `Parser` across
//! parses — `Parser::new` allocates the scanner state and binding the
//! grammar via `set_language` is the dominant per-call cost. The
//! indexer's hot loop runs once per source file (13 k+ files on the
//! reference repo), so re-creating both per file was the single
//! biggest amplifier flagged by the perf review.
//!
//! The pool keys by the canonical lang slug (`&'static str`) so
//! lookup is a small `HashMap` probe — at most 7 entries per worker.
//! `Parser` is `!Sync`, so a `thread_local!` `RefCell` is the right
//! shape: rayon-spawned indexer workers each get their own pool, and
//! the borrow lifetime is bounded by the synchronous `parse` call.

use anyhow::{anyhow, Result};
use std::cell::RefCell;
use std::collections::HashMap;
use tree_sitter::{Language, Parser, Tree};

thread_local! {
    static PARSERS: RefCell<HashMap<&'static str, Parser>> = RefCell::new(HashMap::new());
}

/// Parse `src` with the cached `Parser` for `lang`. The `Parser` is
/// created on first use per worker thread and reused for every
/// subsequent file. Callers receive an owned `Tree` (independent of
/// the `Parser`'s lifetime) and the borrow on the pool is released
/// before this function returns.
pub fn parse(lang: &str, src: &str) -> Result<Tree> {
    let key = intern(lang)?;
    PARSERS.with(|cell| {
        let mut map = cell.borrow_mut();
        if !map.contains_key(key) {
            let ts_lang = ts_language(key)?;
            let mut p = Parser::new();
            p.set_language(&ts_lang)
                .map_err(|e| anyhow!("set_language: {e}"))?;
            map.insert(key, p);
        }
        let parser = map
            .get_mut(key)
            .expect("entry just inserted on the contains_key=false branch");
        parser
            .parse(src, None)
            .ok_or_else(|| anyhow!("parse failed"))
    })
}

/// Test-only accessor: drop every cached `Parser` on the calling
/// thread. Tests that bench cold vs warm parses use this to force a
/// fresh `set_language` allocation.
#[cfg(test)]
pub fn _clear_for_tests() {
    PARSERS.with(|cell| cell.borrow_mut().clear());
}

/// Resolve a user-supplied lang string to the canonical `&'static str`
/// slug used as the pool's key. Keeps every hashmap key on the
/// `'static` interned set so `HashMap<&'static str, _>` is sufficient.
fn intern(lang: &str) -> Result<&'static str> {
    Ok(match lang {
        "typescript" => "typescript",
        "tsx" => "tsx",
        "javascript" => "javascript",
        "ruby" => "ruby",
        "rust" => "rust",
        "go" => "go",
        "python" => "python",
        _ => return Err(anyhow!("unsupported lang: {lang}")),
    })
}

fn ts_language(lang: &str) -> Result<Language> {
    Ok(match lang {
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        _ => return Err(anyhow!("unsupported lang: {lang}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_lang_and_caches_parser() {
        _clear_for_tests();
        let tree1 = parse("rust", "fn main() {}").unwrap();
        assert_eq!(tree1.root_node().kind(), "source_file");
        // Second call must hit the cache; behavior should be identical.
        let tree2 = parse("rust", "fn other() {}").unwrap();
        assert_eq!(tree2.root_node().kind(), "source_file");
        // Pool should hold exactly one entry for `"rust"`.
        PARSERS.with(|cell| {
            let map = cell.borrow();
            assert_eq!(map.len(), 1);
            assert!(map.contains_key("rust"));
        });
    }

    #[test]
    fn distinct_langs_get_distinct_parsers() {
        _clear_for_tests();
        parse("rust", "fn a() {}").unwrap();
        parse("python", "def a():\n    pass\n").unwrap();
        parse("go", "package x\nfunc A() {}").unwrap();
        PARSERS.with(|cell| assert_eq!(cell.borrow().len(), 3));
    }

    #[test]
    fn unsupported_lang_returns_err() {
        let err = parse("cobol", "DISPLAY 'hi'.").unwrap_err();
        assert!(err.to_string().contains("unsupported lang"));
    }
}
