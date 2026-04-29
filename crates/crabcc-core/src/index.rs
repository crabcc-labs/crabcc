use crate::{extract, hash, store::Store, walker};
use anyhow::Result;
use serde::Serialize;
use std::collections::HashSet;
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

#[derive(Debug, Default, Serialize)]
pub struct RefreshStats {
    pub new: usize,
    pub reindexed: usize,
    pub touched: usize,
    pub unchanged: usize,
    pub deleted: usize,
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
            None => {
                stats.skipped_unsupported += 1;
                continue;
            }
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => {
                stats.skipped_unreadable += 1;
                continue;
            }
        };
        if bytes.len() > MAX_FILE_BYTES {
            stats.skipped_too_large += 1;
            continue;
        }
        let src = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(_) => {
                stats.skipped_unreadable += 1;
                continue;
            }
        };

        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();

        let symbols = match extract::extract_file(&rel, src, lang) {
            Ok(s) => s,
            Err(_) => {
                stats.skipped_parse_error += 1;
                continue;
            }
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

/// Wipe the index and rebuild from scratch. Use this when the schema or
/// extractor rules change and the existing index is stale by construction.
pub fn full_index(root: &Path, store: &Store) -> Result<IndexStats> {
    store.clear_all()?;
    build_index(root, store)
}

/// Incrementally update the index against the current state of `root`.
///
/// Strategy per file:
///   - mtime unchanged   → skip (cheapest path, no read)
///   - mtime changed     → hash bytes
///       - hash matches  → just update mtime (touched)
///       - hash differs  → reparse + replace symbols
///   - file new on disk  → index it
///   - file gone on disk → delete its row (cascades to symbols)
pub fn refresh(root: &Path, store: &Store) -> Result<RefreshStats> {
    let mut stats = RefreshStats::default();
    let in_db = store.list_files_with_meta()?;
    let mut seen: HashSet<String> = HashSet::with_capacity(in_db.len());

    for path in walker::walk_repo(root) {
        let lang = match extract::detect_lang(&path) {
            Some(l) => l,
            None => {
                stats.skipped_unsupported += 1;
                continue;
            }
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        seen.insert(rel.clone());

        let mtime = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        if let Some((stored_sha, stored_mtime)) = in_db.get(&rel) {
            if *stored_mtime == mtime {
                stats.unchanged += 1;
                continue;
            }
            // mtime changed — read and hash to decide.
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => {
                    stats.skipped_unreadable += 1;
                    continue;
                }
            };
            if bytes.len() > MAX_FILE_BYTES {
                stats.skipped_too_large += 1;
                continue;
            }
            let sha = hash::sha256_hex(&bytes);
            if &sha == stored_sha {
                store.touch_mtime(&rel, mtime)?;
                stats.touched += 1;
                continue;
            }
            // Real content change — reindex.
            let src = match std::str::from_utf8(&bytes) {
                Ok(s) => s,
                Err(_) => {
                    stats.skipped_unreadable += 1;
                    continue;
                }
            };
            let symbols = match extract::extract_file(&rel, src, lang) {
                Ok(s) => s,
                Err(_) => {
                    stats.skipped_parse_error += 1;
                    continue;
                }
            };
            let file_id = store.upsert_file(&rel, &sha, mtime, lang)?;
            store.replace_symbols(file_id, &symbols)?;
            stats.reindexed += 1;
        } else {
            // New file on disk.
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => {
                    stats.skipped_unreadable += 1;
                    continue;
                }
            };
            if bytes.len() > MAX_FILE_BYTES {
                stats.skipped_too_large += 1;
                continue;
            }
            let src = match std::str::from_utf8(&bytes) {
                Ok(s) => s,
                Err(_) => {
                    stats.skipped_unreadable += 1;
                    continue;
                }
            };
            let symbols = match extract::extract_file(&rel, src, lang) {
                Ok(s) => s,
                Err(_) => {
                    stats.skipped_parse_error += 1;
                    continue;
                }
            };
            let sha = hash::sha256_hex(&bytes);
            let file_id = store.upsert_file(&rel, &sha, mtime, lang)?;
            store.replace_symbols(file_id, &symbols)?;
            stats.new += 1;
        }
    }

    // Delete rows for files no longer on disk.
    for rel in in_db.keys() {
        if !seen.contains(rel) {
            store.delete_file(rel)?;
            stats.deleted += 1;
        }
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
        write(
            &root.join("hello.ts"),
            "export function hello(name: string) { return name; }",
        );
        write(
            &root.join("user.rb"),
            "class User\n  def name; @name; end\nend\n",
        );
        write(&root.join("README.md"), "ignored");

        let store = Store::open(&root.join("idx.db")).unwrap();
        let stats = build_index(root, &store).unwrap();

        assert_eq!(stats.files_indexed, 2, "stats: {stats:?}");
        assert!(
            stats.symbols >= 3,
            "expected ≥3 symbols, got {}",
            stats.symbols
        );
        assert!(
            stats.skipped_unsupported >= 1,
            "README.md + idx.db should skip"
        );

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

    fn fresh_repo_with(files: &[(&str, &str)]) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (name, body) in files {
            write(&root.join(name), body);
        }
        let store = Store::open(&root.join("idx.db")).unwrap();
        build_index(root, &store).unwrap();
        (dir, store)
    }

    #[test]
    fn refresh_no_changes_marks_all_unchanged() {
        let (dir, store) = fresh_repo_with(&[
            ("a.ts", "export function a(){return 1;}"),
            ("b.rb", "class B; end\n"),
        ]);
        let stats = refresh(dir.path(), &store).unwrap();
        assert_eq!(stats.unchanged, 2, "stats: {stats:?}");
        assert_eq!(stats.reindexed, 0);
        assert_eq!(stats.new, 0);
        assert_eq!(stats.deleted, 0);
    }

    #[test]
    fn refresh_picks_up_modified_file() {
        let (dir, store) = fresh_repo_with(&[("a.ts", "export function a(){return 1;}")]);
        // Force a perceptibly different mtime + content.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write(
            &dir.path().join("a.ts"),
            "export function a(){return 1;}\nexport function b(){return 2;}\n",
        );

        let stats = refresh(dir.path(), &store).unwrap();
        assert_eq!(stats.reindexed, 1, "stats: {stats:?}");
        assert!(store.find_by_name("b").unwrap().len() == 1);
    }

    #[test]
    fn refresh_picks_up_new_file() {
        let (dir, store) = fresh_repo_with(&[("a.ts", "export function a(){return 1;}")]);
        write(&dir.path().join("c.ts"), "export function c(){return 3;}");

        let stats = refresh(dir.path(), &store).unwrap();
        assert_eq!(stats.new, 1, "stats: {stats:?}");
        assert_eq!(store.find_by_name("c").unwrap().len(), 1);
    }

    #[test]
    fn refresh_deletes_missing_file() {
        let (dir, store) = fresh_repo_with(&[
            ("a.ts", "export function a(){return 1;}"),
            ("b.ts", "export function b(){return 2;}"),
        ]);
        std::fs::remove_file(dir.path().join("b.ts")).unwrap();

        let stats = refresh(dir.path(), &store).unwrap();
        assert_eq!(stats.deleted, 1, "stats: {stats:?}");
        assert_eq!(store.find_by_name("b").unwrap().len(), 0);
        assert_eq!(store.find_by_name("a").unwrap().len(), 1);
    }

    #[test]
    fn full_index_wipes_then_rebuilds() {
        let (dir, store) = fresh_repo_with(&[("a.ts", "export function a(){return 1;}")]);
        std::fs::remove_file(dir.path().join("a.ts")).unwrap();
        write(&dir.path().join("z.ts"), "export function z(){return 9;}");

        let stats = full_index(dir.path(), &store).unwrap();
        assert_eq!(stats.files_indexed, 1, "stats: {stats:?}");
        assert_eq!(store.find_by_name("a").unwrap().len(), 0);
        assert_eq!(store.find_by_name("z").unwrap().len(), 1);
    }

    #[test]
    fn git_worktree_isolation() {
        // Verify that two checkouts of the "same repo" (independent tempdirs
        // simulating worktrees) maintain entirely independent indexes.
        //
        // Real `git worktree add` creates a `.git` *file* (not directory) at
        // the worktree root pointing at `<main-repo>/.git/worktrees/<name>`.
        // The `ignore` crate handles that via libgit2 semantics; tested
        // implicitly by walker.rs::respects_gitignore. Here we focus on the
        // crabcc-level invariant: two roots = two indexes, no cross-talk.
        //
        // (This test deliberately does NOT also check refresh-after-edit —
        // that's covered by `refresh_picks_up_modified_file` and only adds
        // tempdir-mtime-granularity flake to this test without strengthening
        // the property under test.)
        let main = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();

        write(
            &main.path().join("shared.ts"),
            "export function origin(){return 1;}",
        );
        write(
            &work.path().join("shared.ts"),
            "export function origin(){return 1;}\n\
               export function feature(){return 2;}",
        );

        let main_store = Store::open(&main.path().join("idx.db")).unwrap();
        let work_store = Store::open(&work.path().join("idx.db")).unwrap();
        build_index(main.path(), &main_store).unwrap();
        build_index(work.path(), &work_store).unwrap();

        // `feature` exists only in the worktree's checkout.
        assert_eq!(main_store.find_by_name("feature").unwrap().len(), 0);
        assert_eq!(work_store.find_by_name("feature").unwrap().len(), 1);
        // `origin` exists in both — that's correct, they're separate trees.
        assert_eq!(main_store.find_by_name("origin").unwrap().len(), 1);
        assert_eq!(work_store.find_by_name("origin").unwrap().len(), 1);

        // The two stores must hold disjoint file rowids (different SQLite files).
        let main_files = main_store.list_files().unwrap();
        let work_files = work_store.list_files().unwrap();
        assert_eq!(main_files.len(), 1);
        assert_eq!(work_files.len(), 1);
    }
}
