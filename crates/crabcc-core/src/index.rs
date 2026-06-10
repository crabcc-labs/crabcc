use crate::{extract, hash, store::Store, walker};
use ahash::HashSet;
use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

#[derive(Debug, Default, Serialize)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub symbols: usize,
    pub edges: usize,
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

/// What changed since the last `refresh`. Use this when an agent already
/// has the previous state cached and only needs to re-read the diff.
///
/// `added`    — files freshly indexed (not in the DB before this call).
/// `modified` — existing files whose bytes changed (mtime + sha both differ).
/// `removed`  — files that were in the DB but no longer exist on disk.
///
/// `touched` (mtime bumped, content unchanged) is intentionally NOT in
/// this list — agents care about *content* deltas, not metadata.
#[derive(Debug, Default, Serialize)]
pub struct RefreshDelta {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub removed: Vec<String>,
    pub stats: RefreshStats,
}

const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

/// A single file's extracted content, ready to write into SQLite.
struct ExtractedFile {
    rel: String,
    sha: String,
    mtime: i64,
    lang: &'static str,
    symbols: Vec<crate::types::Symbol>,
    edges: Vec<crate::types::Edge>,
}

enum FileOutcome {
    Extracted(ExtractedFile),
    SkippedUnsupported,
    SkippedUnreadable,
    SkippedTooLarge,
    SkippedParseError,
}

pub fn build_index(root: &Path, store: &Store) -> Result<IndexStats> {
    build_index_with_progress(root, store, None)
}

pub fn build_index_with_progress(
    root: &Path,
    store: &Store,
    progress: Option<Arc<AtomicUsize>>,
) -> Result<IndexStats> {
    let files: Vec<_> = walker::walk_repo(root).collect();
    let total = files.len();

    // Phase 1 — parallel extraction (CPU-bound tree-sitter parsing).
    // SQLite writes are NOT here; Store is not Sync.
    let outcomes: Vec<FileOutcome> = files
        .into_par_iter()
        .map(|path| {
            let lang = match extract::detect_lang(&path) {
                Some(l) => l,
                None => return FileOutcome::SkippedUnsupported,
            };
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => return FileOutcome::SkippedUnreadable,
            };
            if bytes.len() > MAX_FILE_BYTES {
                return FileOutcome::SkippedTooLarge;
            }
            let src = match std::str::from_utf8(&bytes) {
                Ok(s) => s,
                Err(_) => return FileOutcome::SkippedUnreadable,
            };
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let (symbols, edges) = match extract::extract_file_with_edges(&rel, src, lang) {
                Ok(pair) => pair,
                Err(_) => return FileOutcome::SkippedParseError,
            };
            let sha = hash::sha256_hex(&bytes);
            let mtime = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or_default();
            FileOutcome::Extracted(ExtractedFile {
                rel,
                sha,
                mtime,
                lang,
                symbols,
                edges,
            })
        })
        .collect();

    // Phase 2 — sequential SQLite writes.
    let mut stats = IndexStats::default();
    let mut done = 0usize;
    for outcome in outcomes {
        match outcome {
            FileOutcome::Extracted(f) => {
                let file_id = store.upsert_file(&f.rel, &f.sha, f.mtime, f.lang)?;
                store.replace_symbols(file_id, &f.symbols)?;
                store.replace_edges(file_id, &f.edges)?;
                stats.files_indexed += 1;
                stats.symbols += f.symbols.len();
                stats.edges += f.edges.len();
            }
            FileOutcome::SkippedUnsupported => stats.skipped_unsupported += 1,
            FileOutcome::SkippedUnreadable => stats.skipped_unreadable += 1,
            FileOutcome::SkippedTooLarge => stats.skipped_too_large += 1,
            FileOutcome::SkippedParseError => stats.skipped_parse_error += 1,
        }
        done += 1;
        if let Some(ref p) = progress {
            p.store(done, Ordering::Relaxed);
        }
    }
    let _ = total; // reported via progress or caller
    Ok(stats)
}

/// Wipe the index and rebuild from scratch. Use this when the schema or
/// extractor rules change and the existing index is stale by construction.
///
/// Flips the `edges_populated` meta flag to '1' on success — that gate is
/// what `query::query_callers` checks before taking the SQL-only fast path.
/// Refresh deliberately doesn't touch the flag: if it was '1', edges stayed
/// consistent through the per-file replace; if it was '0' (v1.0.0 upgrade),
/// only changed files got edges and the SQL path would lie.
pub fn full_index(root: &Path, store: &Store) -> Result<IndexStats> {
    let started = std::time::Instant::now();
    tracing::info!(target: "crabcc_core::index", path = %root.display(), "full_index: start");
    store.clear_all()?;

    // Progress ticker — prints to stderr every 500 ms while indexing.
    // Gated by CRABCC_PROGRESS env var: "0"/"false" suppresses it,
    // anything else (or absent) enables it when stderr is a tty.
    let show_progress = std::env::var("CRABCC_PROGRESS")
        .map(|v| !matches!(v.as_str(), "0" | "false" | "no"))
        .unwrap_or(true)
        && atty::is(atty::Stream::Stderr);

    let progress = Arc::new(AtomicUsize::new(0));
    let ticker = if show_progress {
        let p = Arc::clone(&progress);
        let ticker = std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let n = p.load(Ordering::Relaxed);
            if n == usize::MAX {
                break;
            }
            eprint!("\r  indexing… {} files", n);
            let _ = std::io::Write::flush(&mut std::io::stderr());
        });
        Some(ticker)
    } else {
        None
    };

    let stats = build_index_with_progress(root, store, Some(Arc::clone(&progress)))?;

    // Signal ticker to stop, clear progress line.
    progress.store(usize::MAX, Ordering::Relaxed);
    if let Some(t) = ticker {
        let _ = t.join();
        eprint!("\r\x1b[K"); // erase line
    }

    store.meta_set("edges_populated", "1")?;
    tracing::info!(
        target: "crabcc_core::index",
        files = stats.files_indexed,
        symbols = stats.symbols,
        edges = stats.edges,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "full_index: done"
    );
    Ok(stats)
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
    Ok(refresh_delta(root, store)?.stats)
}

/// Same logic as [`refresh`], but additionally returns the per-bucket
/// file lists (`added` / `modified` / `removed`). New surface for agents
/// that want to re-read only what changed.
/// Result of `persist_file`: the file was indexed, or skipped for a reason.
/// (Distinct from `FileOutcome`, which the parallel `build_index` path uses to
/// carry extracted content back for sequential writes.)
enum PersistOutcome {
    Indexed,
    Unreadable,
    ParseError,
}

/// utf8-decode `bytes`, extract symbols/edges, and persist them (upsert file +
/// replace symbols/edges). Returns whether the file was indexed or why it was
/// skipped, so the "modified" and "new" arms of `refresh_delta` share this body
/// and differ only in their stats/list bookkeeping. Callers must have already
/// applied the `MAX_FILE_BYTES` size cap.
fn persist_file(
    store: &Store,
    rel: &str,
    lang: &str,
    bytes: &[u8],
    sha: &str,
    mtime: i64,
) -> Result<PersistOutcome> {
    let src = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return Ok(PersistOutcome::Unreadable),
    };
    let (symbols, edges) = match extract::extract_file_with_edges(rel, src, lang) {
        Ok(pair) => pair,
        Err(_) => return Ok(PersistOutcome::ParseError),
    };
    let file_id = store.upsert_file(rel, sha, mtime, lang)?;
    store.replace_symbols(file_id, &symbols)?;
    store.replace_edges(file_id, &edges)?;
    Ok(PersistOutcome::Indexed)
}

pub fn refresh_delta(root: &Path, store: &Store) -> Result<RefreshDelta> {
    let started = std::time::Instant::now();
    tracing::info!(target: "crabcc_core::index", path = %root.display(), "refresh_delta: start");
    let mut delta = RefreshDelta::default();
    let in_db = store.list_files_with_meta()?;
    let mut seen: HashSet<String> =
        HashSet::with_capacity_and_hasher(in_db.len(), Default::default());

    for path in walker::walk_repo(root) {
        let lang = match extract::detect_lang(&path) {
            Some(l) => l,
            None => {
                delta.stats.skipped_unsupported += 1;
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
            .unwrap_or_default();

        if let Some((stored_sha, stored_mtime)) = in_db.get(&rel) {
            if *stored_mtime == mtime {
                delta.stats.unchanged += 1;
                continue;
            }
            // mtime changed — read and hash to decide.
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => {
                    delta.stats.skipped_unreadable += 1;
                    continue;
                }
            };
            if bytes.len() > MAX_FILE_BYTES {
                delta.stats.skipped_too_large += 1;
                continue;
            }
            let sha = hash::sha256_hex(&bytes);
            if &sha == stored_sha {
                store.touch_mtime(&rel, mtime)?;
                delta.stats.touched += 1;
                continue;
            }
            // Real content change — reindex.
            match persist_file(store, &rel, lang, &bytes, &sha, mtime)? {
                PersistOutcome::Indexed => {
                    delta.stats.reindexed += 1;
                    delta.modified.push(rel);
                }
                PersistOutcome::Unreadable => delta.stats.skipped_unreadable += 1,
                PersistOutcome::ParseError => delta.stats.skipped_parse_error += 1,
            }
        } else {
            // New file on disk.
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => {
                    delta.stats.skipped_unreadable += 1;
                    continue;
                }
            };
            if bytes.len() > MAX_FILE_BYTES {
                delta.stats.skipped_too_large += 1;
                continue;
            }
            let sha = hash::sha256_hex(&bytes);
            match persist_file(store, &rel, lang, &bytes, &sha, mtime)? {
                PersistOutcome::Indexed => {
                    delta.stats.new += 1;
                    delta.added.push(rel);
                }
                PersistOutcome::Unreadable => delta.stats.skipped_unreadable += 1,
                PersistOutcome::ParseError => delta.stats.skipped_parse_error += 1,
            }
        }
    }

    // Delete rows for files no longer on disk.
    for rel in in_db.keys().filter(|r| !seen.contains(*r)) {
        store.delete_file(rel)?;
        delta.stats.deleted += 1;
        delta.removed.push(rel.clone());
    }

    // Sort each bucket so the JSON output is deterministic — matters for
    // the fingerprint feature and for diffing across calls.
    delta.added.sort_unstable();
    delta.modified.sort_unstable();
    delta.removed.sort_unstable();

    tracing::info!(
        target: "crabcc_core::index",
        added = delta.added.len(),
        modified = delta.modified.len(),
        removed = delta.removed.len(),
        unchanged = delta.stats.unchanged,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "refresh_delta: done"
    );
    Ok(delta)
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
        write(&root.join("notes.txt"), "ignored");

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
            "notes.txt + idx.db should skip"
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

    #[test]
    fn parallel_build_indexes_multilang_handles_skips_and_is_deterministic() {
        // Edge-case guard for the parallel parse path: a mix of languages
        // plus every skip reason in one build. The parallel parse must
        // index every valid file, tally each skip bucket, and produce a
        // byte-stable result run-to-run (par_iter().collect() preserves
        // walk order, so symbol/edge counts don't depend on thread timing).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("a.rs"),
            "pub struct Widget { id: u32 }\npub fn use_widget(w: &Widget) -> u32 { w.id }\n",
        );
        write(
            &root.join("b.py"),
            "class Service:\n    def run(self):\n        return 1\n",
        );
        write(
            &root.join("c.ts"),
            "export function handler() { return 1; }\n",
        );
        write(
            &root.join("d.go"),
            "package x\nfunc Serve() int { return 0 }\n",
        );
        write(&root.join("notes.txt"), "docs only, unsupported\n"); // skipped_unsupported
        write(&root.join("README.md"), "# Docs\n\n## Usage\n"); // markdown is indexed since the C/Zig/data-format batch
        write(
            &root.join("big.rs"),
            &"// pad\n".repeat(MAX_FILE_BYTES / 7 + 100),
        ); // skipped_too_large
        std::fs::write(root.join("raw.rs"), [0xff_u8, 0xfe, 0x00, 0x9f]).unwrap(); // non-utf8 -> unreadable

        let store = Store::open(&root.join("idx.db")).unwrap();
        let stats = full_index(root, &store).unwrap();

        assert_eq!(stats.files_indexed, 5, "stats: {stats:?}");
        assert_eq!(stats.skipped_too_large, 1, "stats: {stats:?}");
        assert!(stats.skipped_unsupported >= 1, "stats: {stats:?}");
        assert_eq!(stats.skipped_unreadable, 1, "non-utf8 file: {stats:?}");

        // Cross-language symbols all landed (incl. the markdown heading).
        for name in ["Widget", "Service", "handler", "Serve", "Usage"] {
            assert_eq!(
                store.find_by_name(name).unwrap().len(),
                1,
                "missing {name} after parallel index"
            );
        }

        // Determinism: a second full rebuild yields identical counts.
        let stats2 = full_index(root, &store).unwrap();
        assert_eq!(stats.files_indexed, stats2.files_indexed);
        assert_eq!(
            stats.symbols, stats2.symbols,
            "symbol count not stable across rebuilds"
        );
        assert_eq!(
            stats.edges, stats2.edges,
            "edge count not stable across rebuilds"
        );
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
    #[ignore = "slow (~1.3s) — full Tantivy + SQLite refresh; run locally with --ignored"]
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

    // ---- refresh_delta (feature 1: --delta) -------------------------------

    #[test]
    fn refresh_delta_no_changes_yields_empty_lists() {
        let (dir, store) = fresh_repo_with(&[
            ("a.ts", "export function a(){return 1;}"),
            ("b.rb", "class B; end\n"),
        ]);
        let d = refresh_delta(dir.path(), &store).unwrap();
        assert!(d.added.is_empty(), "added: {:?}", d.added);
        assert!(d.modified.is_empty(), "modified: {:?}", d.modified);
        assert!(d.removed.is_empty(), "removed: {:?}", d.removed);
        assert_eq!(d.stats.unchanged, 2);
    }

    #[test]
    #[ignore = "slow (~1.4s) — full Tantivy + SQLite refresh; run locally with --ignored"]
    fn refresh_delta_captures_added_modified_removed() {
        let (dir, store) = fresh_repo_with(&[
            ("a.ts", "export function a(){return 1;}"),
            ("b.ts", "export function b(){return 2;}"),
        ]);
        // Mutate: modify a.ts (force mtime drift), add c.ts, delete b.ts.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write(
            &dir.path().join("a.ts"),
            "export function a(){return 1;}\nexport function aa(){return 11;}\n",
        );
        write(&dir.path().join("c.ts"), "export function c(){return 3;}");
        std::fs::remove_file(dir.path().join("b.ts")).unwrap();

        let d = refresh_delta(dir.path(), &store).unwrap();
        assert_eq!(d.added, vec!["c.ts"], "added: {:?}", d.added);
        assert_eq!(d.modified, vec!["a.ts"], "modified: {:?}", d.modified);
        assert_eq!(d.removed, vec!["b.ts"], "removed: {:?}", d.removed);

        // Stats and lists must agree: counts == list lengths.
        assert_eq!(d.stats.new, d.added.len());
        assert_eq!(d.stats.reindexed, d.modified.len());
        assert_eq!(d.stats.deleted, d.removed.len());
    }

    #[test]
    #[ignore = "slow (~1.2s) — full Tantivy + SQLite refresh; run locally with --ignored"]
    fn refresh_delta_excludes_touched_only_files() {
        // A file whose mtime bumped but content is identical is `touched`,
        // not `modified`. Agents that already have the cached body shouldn't
        // be told to re-read it.
        let (dir, store) = fresh_repo_with(&[("a.ts", "export function a(){return 1;}")]);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        // Re-write byte-identical content — mtime advances, sha doesn't.
        write(&dir.path().join("a.ts"), "export function a(){return 1;}");

        let d = refresh_delta(dir.path(), &store).unwrap();
        assert!(d.modified.is_empty(), "modified: {:?}", d.modified);
        assert_eq!(d.stats.touched, 1, "stats: {:?}", d.stats);
    }

    #[test]
    fn refresh_delta_buckets_are_sorted() {
        // Determinism contract — output order must not depend on walk order
        // or HashSet iteration order. Required for the fingerprint feature
        // (#4) to produce stable hashes across runs.
        let (dir, store) = fresh_repo_with(&[("z.ts", "export function z(){return 9;}")]);
        write(&dir.path().join("a.ts"), "export function a(){return 1;}");
        write(&dir.path().join("m.ts"), "export function m(){return 5;}");
        write(&dir.path().join("c.ts"), "export function c(){return 3;}");

        let d = refresh_delta(dir.path(), &store).unwrap();
        let sorted: Vec<String> = {
            let mut v = d.added.clone();
            v.sort_unstable();
            v
        };
        assert_eq!(d.added, sorted, "added must be sorted: {:?}", d.added);
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
