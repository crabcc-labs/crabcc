use crate::{extract, hash, store::Store, walker};
use anyhow::Result;
use serde::Serialize;
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Debug, Default, Serialize)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub symbols: usize,
    pub skipped_unsupported: usize,
    pub skipped_too_large: usize,
    pub skipped_unreadable: usize,
    pub skipped_parse_error: usize,
}

const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

pub fn build_index(root: &Path, store: &Store) -> Result<IndexStats> {
    let mut stats = IndexStats::default();

    for path in walker::walk_repo(root) {
        let lang = match extract::detect_lang(&path) {
            Some(l) => l,
            None => { stats.skipped_unsupported += 1; continue; }
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => { stats.skipped_unreadable += 1; continue; }
        };
        if bytes.len() > MAX_FILE_BYTES {
            stats.skipped_too_large += 1;
            continue;
        }
        let src = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(_) => { stats.skipped_unreadable += 1; continue; }
        };

        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();

        let symbols = match extract::extract_file(&rel, src, lang) {
            Ok(s) => s,
            Err(_) => { stats.skipped_parse_error += 1; continue; }
        };

        let sha = hash::sha256_hex(&bytes);
        let mtime = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let file_id = store.upsert_file(&rel, &sha, mtime, lang)?;
        store.replace_symbols(file_id, &symbols)?;
        stats.files_indexed += 1;
        stats.symbols += symbols.len();
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, body: &str) {
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn smoke_index_typescript_and_ruby() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("hello.ts"),
              "export function hello(name: string) { return name; }");
        write(&root.join("user.rb"),
              "class User\n  def name; @name; end\nend\n");
        write(&root.join("README.md"), "ignored");

        let store = Store::open(&root.join("idx.db")).unwrap();
        let stats = build_index(root, &store).unwrap();

        assert_eq!(stats.files_indexed, 2, "stats: {stats:?}");
        assert!(stats.symbols >= 3, "expected ≥3 symbols, got {}", stats.symbols);
        assert!(stats.skipped_unsupported >= 1, "README.md + idx.db should skip");

        let hello = store.find_by_name("hello").unwrap();
        assert_eq!(hello.len(), 1);
        assert_eq!(hello[0].file, "hello.ts");

        let user = store.find_by_name("User").unwrap();
        assert_eq!(user.len(), 1);
        assert_eq!(user[0].file, "user.rb");
    }

    #[test]
    fn skips_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let big = "// pad\n".repeat(MAX_FILE_BYTES / 7 + 100);
        write(&root.join("big.ts"), &big);

        let store = Store::open(&root.join("idx.db")).unwrap();
        let stats = build_index(root, &store).unwrap();
        assert_eq!(stats.skipped_too_large, 1);
        assert_eq!(stats.files_indexed, 0);
    }
}
