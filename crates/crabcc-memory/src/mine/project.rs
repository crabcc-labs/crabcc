//! Project miner — drawer per text file under a repo root.
//!
//! Reuses `crabcc_core::walker::walk_repo` for ignore-aware traversal
//! (gitignore, hidden, binary-by-extension). Body is the raw file
//! contents; `wing="proj"`, `source_id="proj:<repo-relative-path>"`.
//! The Palace's existing `(source_id, sha256)` UNIQUE constraint makes
//! repeat mines idempotent — only files whose contents changed since
//! the last run land as fresh rows.

use super::{MineReport, SkipReason};
use crate::Palace;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Default ceiling on per-file body bytes. Files larger than this are
/// skipped — embedding a 5 MB minified JSON does not help recall and
/// blows up the FTS5 index. Override via [`MineProjectOpts::max_bytes`].
pub const DEFAULT_MAX_FILE_BYTES: u64 = 1_000_000;

/// First-N bytes scanned for NUL when classifying binary vs text.
const BINARY_PROBE_BYTES: usize = 8 * 1024;

/// Knobs for [`mine_project`]. Constructed via `MineProjectOpts::default()`
/// and overridden field-by-field; new fields are always purely additive.
#[derive(Debug, Clone)]
pub struct MineProjectOpts {
    pub max_bytes: u64,
    pub session_id: Option<String>,
}

impl Default for MineProjectOpts {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_FILE_BYTES,
            session_id: None,
        }
    }
}

/// Walk `path` (must be a directory) and emit a drawer per text file.
/// `path` doubles as the strip-prefix for the `source_id` so a mine
/// rooted at the repo top yields short keys like `proj:src/lib.rs`.
pub fn mine_project(palace: &Palace, path: &Path, opts: &MineProjectOpts) -> Result<MineReport> {
    let mut report = MineReport::default();

    for abs in crabcc_core::walker::walk_repo(path) {
        report.scanned += 1;

        // The walker already excludes `.git`, hidden dotfiles, and
        // gitignored paths; explicitly drop the `.crabcc` cache dir
        // since users sometimes commit a stray index.db.
        if path_passes_through(&abs, ".crabcc") {
            report.record_skip();
            continue;
        }

        let rel = abs.strip_prefix(path).unwrap_or(&abs);
        let rel_str = rel.to_string_lossy().into_owned();

        match read_text_file(&abs, opts.max_bytes) {
            Ok(Some(body)) => {
                if body.trim().is_empty() {
                    log_skip(&rel_str, SkipReason::Empty);
                    report.record_skip();
                    continue;
                }
                ingest(
                    palace,
                    &rel_str,
                    &body,
                    opts.session_id.as_deref(),
                    &mut report,
                )?;
            }
            Ok(None) => {
                // Returned None → caller-classified skip; reason already logged.
                report.record_skip();
            }
            Err(err) => {
                tracing::debug!(target: "crabcc_memory::mine", "skip {rel_str}: {err}");
                report.record_skip();
            }
        }
    }

    Ok(report)
}

fn ingest(
    palace: &Palace,
    rel_str: &str,
    body: &str,
    session: Option<&str>,
    report: &mut MineReport,
) -> Result<()> {
    let source_id = format!("proj:{rel_str}");
    let pre_count = palace.count()?;
    palace.remember_in_session("proj", None, &source_id, body, session)?;
    let post_count = palace.count()?;
    if post_count > pre_count {
        report.record_inserted();
    } else {
        report.record_dedup();
    }
    Ok(())
}

fn read_text_file(path: &Path, max_bytes: u64) -> Result<Option<String>> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > max_bytes {
        log_skip(&path.display().to_string(), SkipReason::OverSize);
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    if looks_binary(&bytes) {
        log_skip(&path.display().to_string(), SkipReason::Binary);
        return Ok(None);
    }
    // Lossy decode keeps the miner running on files with stray non-UTF8
    // bytes (rare in source, common in fixtures); BM25 still works on
    // the replacement-char-sprinkled string.
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn looks_binary(bytes: &[u8]) -> bool {
    let probe_len = bytes.len().min(BINARY_PROBE_BYTES);
    bytes[..probe_len].contains(&0)
}

fn path_passes_through(p: &Path, segment: &str) -> bool {
    p.components()
        .any(|c| c.as_os_str().to_string_lossy() == segment)
}

fn log_skip(path: &str, reason: SkipReason) {
    tracing::debug!(target: "crabcc_memory::mine", "skip {path}: {reason:?}");
}

/// Helper for tests + bench harnesses that want a synthetic repo on disk.
/// Writes one small text file per pair under `dir`. Pairs are
/// `(rel-path, contents)`. Existing files are overwritten.
pub fn write_synthetic_repo<P: AsRef<Path>>(dir: P, pairs: &[(&str, &str)]) -> Result<PathBuf> {
    let dir = dir.as_ref();
    for (rel, body) in pairs {
        let abs = dir.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&abs, body)?;
    }
    Ok(dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn mines_text_files_and_skips_dotcrabcc() {
        let dir = tempdir().unwrap();
        write_synthetic_repo(
            dir.path(),
            &[
                ("README.md", "# hello world\n"),
                ("src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 { a + b }"),
                (".crabcc/index.db", "should be skipped"),
            ],
        )
        .unwrap();
        let palace = Palace::ephemeral();

        let report = mine_project(&palace, dir.path(), &MineProjectOpts::default()).unwrap();

        assert_eq!(report.inserted, 2, "exactly README.md + src/lib.rs land");
        assert_eq!(palace.count().unwrap(), 2);
        let drawers = palace.list_drawers(Some("proj"), 10).unwrap();
        let sources: Vec<_> = drawers.iter().map(|d| d.source_id.as_str()).collect();
        assert!(sources.iter().any(|s| s.ends_with("README.md")));
        assert!(sources.iter().any(|s| s.ends_with("lib.rs")));
        assert!(!sources.iter().any(|s| s.contains(".crabcc")));
    }

    #[test]
    fn rerun_is_idempotent() {
        let dir = tempdir().unwrap();
        write_synthetic_repo(dir.path(), &[("a.txt", "alpha"), ("b.txt", "beta")]).unwrap();
        let palace = Palace::ephemeral();

        let first = mine_project(&palace, dir.path(), &MineProjectOpts::default()).unwrap();
        let second = mine_project(&palace, dir.path(), &MineProjectOpts::default()).unwrap();

        assert_eq!(first.inserted, 2);
        assert_eq!(second.inserted, 0);
        assert_eq!(second.deduped, 2);
        assert_eq!(palace.count().unwrap(), 2);
    }

    #[test]
    fn skips_oversize_files() {
        let dir = tempdir().unwrap();
        // 2 KB body, cap at 1 KB → skipped.
        let big = "a".repeat(2048);
        write_synthetic_repo(dir.path(), &[("big.txt", &big), ("small.txt", "ok")]).unwrap();
        let palace = Palace::ephemeral();

        let opts = MineProjectOpts {
            max_bytes: 1024,
            ..Default::default()
        };
        let report = mine_project(&palace, dir.path(), &opts).unwrap();

        assert_eq!(report.inserted, 1);
        assert_eq!(report.skipped, 1);
        let only = palace.list_drawers(Some("proj"), 10).unwrap();
        assert_eq!(only.len(), 1);
        assert!(only[0].source_id.ends_with("small.txt"));
    }

    #[test]
    fn skips_binary_files() {
        let dir = tempdir().unwrap();
        let mut bin = b"hello".to_vec();
        bin.push(0); // stuffs a NUL into the probe window
        bin.extend(b"world");
        std::fs::write(dir.path().join("blob.bin"), &bin).unwrap();
        std::fs::write(dir.path().join("doc.txt"), "plain").unwrap();
        let palace = Palace::ephemeral();

        let report = mine_project(&palace, dir.path(), &MineProjectOpts::default()).unwrap();

        assert_eq!(report.inserted, 1);
        assert_eq!(report.skipped, 1);
        let drawers = palace.list_drawers(None, 10).unwrap();
        assert_eq!(drawers.len(), 1);
        assert!(drawers[0].source_id.ends_with("doc.txt"));
    }

    #[test]
    fn empty_files_are_skipped_not_stored() {
        let dir = tempdir().unwrap();
        write_synthetic_repo(
            dir.path(),
            &[("blank.txt", "   \n\t  \n"), ("real.txt", "x")],
        )
        .unwrap();
        let palace = Palace::ephemeral();

        let report = mine_project(&palace, dir.path(), &MineProjectOpts::default()).unwrap();

        assert_eq!(report.inserted, 1);
        assert_eq!(palace.count().unwrap(), 1);
    }

    #[test]
    fn search_round_trip_finds_mined_file() {
        let dir = tempdir().unwrap();
        write_synthetic_repo(
            dir.path(),
            &[
                ("notes.md", "the quick brown fox jumps over the lazy dog"),
                ("other.md", "completely unrelated content here"),
            ],
        )
        .unwrap();
        let palace = Palace::ephemeral();
        mine_project(&palace, dir.path(), &MineProjectOpts::default()).unwrap();

        let hits = palace.search("brown fox", 5).unwrap().hits;
        assert!(
            hits.iter().any(|h| h.source_id.ends_with("notes.md")),
            "expected notes.md in top hits, got {hits:?}"
        );
    }
}
