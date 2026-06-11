//! Outline-stub-aware file read engine. Shared between the CLI's
//! `crabcc read <path>` command and the MCP `crabcc__read` /
//! `crabcc__ctx(tool="read", ...)` tools.
//!
//! Cache key: `(path, session_id)`. Change signal: `sha256(content)`.
//! In `auto` mode a re-read resolves three ways against the cache:
//!
//! - unchanged (same hash) ⇒ outline stub;
//! - changed and we hold the last full body the caller saw ⇒ unified
//!   diff vs that body (diff-on-re-read, #1a) — the caller already has
//!   the old version in context, so a few changed lines beat the whole
//!   file;
//! - changed with no stored baseline (first read, or last served as a
//!   stub/entropy view) ⇒ full content.
//!
//! Every served full / diff body is stored back as the new baseline so
//! the next diff is incremental. `read_count` increments on every call
//! so the loop detector (#5 of the lean-ctx plan) sees re-reads.

use anyhow::{anyhow, Context, Result};
use crabcc_core::{hash::sha256_hex, outline, store::Store};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Ensure the memory db dir + schema exist for `root`, at most once per root
/// per process. `read` only needs the `session_reads` table, but the full
/// `Palace::open` (schema + sqlite-vec + FSST + FTS5 + migrations) costs ~19 ms;
/// running it on every `read` call dominated the MCP `read` tool latency. The
/// schema is idempotent and persistent, so ensuring it once per process is
/// enough — mirrors the once-per-process `register_sqlite_vec_once` pattern.
fn ensure_schema(root: &Path) -> Result<()> {
    static ENSURED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    // Hold the lock across the open so concurrent first-reads (serve_http)
    // don't both pay the ~19 ms open for the same root.
    let mut done = ENSURED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .map_err(|_| anyhow!("poisoned ensure-schema mutex"))?;
    if !done.contains(root) {
        // Idempotent: creates the db dir, applies the schema, migrates legacy.
        let _ = crate::Palace::open(root)?;
        done.insert(root.to_path_buf());
    }
    Ok(())
}

/// Maximum bytes returned in `--mode=full`. 256 KiB ≈ 64 k tokens —
/// generous for source files, trips on assets / generated bundles /
/// test fixtures so a single read doesn't blow the agent's context.
pub const MAX_FULL_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadMode {
    /// Default — cache check; on a re-read serve stub (unchanged), diff
    /// (changed, baseline held), or full (changed, no baseline).
    Auto,
    /// Always serve full content; UPSERTs cache as `full`.
    Full,
    /// Always serve outline stub; UPSERTs cache as `stub`.
    Stub,
    /// Filter lines below an entropy threshold; UPSERTs cache as
    /// `entropy`.
    Entropy,
    /// Force a unified diff vs the last full body served this session.
    /// Falls back to `full` when there is no stored baseline or the file
    /// is unchanged. UPSERTs cache as `diff`.
    Diff,
}

impl ReadMode {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "full" => Ok(Self::Full),
            "stub" => Ok(Self::Stub),
            "entropy" => Ok(Self::Entropy),
            "diff" => Ok(Self::Diff),
            other => Err(anyhow!(
                "--mode must be one of `auto` / `full` / `stub` / `entropy` / `diff`, got {other:?}"
            )),
        }
    }
}

/// Shannon entropy of `text` over its character distribution. Returns
/// 0.0 for the empty string. Unit: bits per character. Reference:
/// random English ≈ 4.5; source code ≈ 3.0–4.0; repetitive log noise
/// (`####...`) drops below 2.0.
pub fn shannon_char_entropy(text: &str) -> f64 {
    let mut freq: std::collections::HashMap<char, usize> = std::collections::HashMap::new();
    let mut total = 0usize;
    for c in text.chars() {
        *freq.entry(c).or_default() += 1;
        total += 1;
    }
    if total == 0 {
        return 0.0;
    }
    let total_f = total as f64;
    freq.values().fold(0.0, |acc, &count| {
        let p = count as f64 / total_f;
        acc - p * p.log2()
    })
}

/// Build the read response. Pure compute path — no stdout. Both the
/// CLI command (`crabcc read`) and the MCP tool (`crabcc__read`)
/// call this and serialize the returned `Value` at the boundary.
pub fn compute(
    root: &Path,
    store: &Store,
    path: PathBuf,
    mode: ReadMode,
    session_id: Option<String>,
    entropy_threshold: f64,
) -> Result<Value> {
    let abs = if path.is_absolute() {
        path.clone()
    } else {
        root.join(&path)
    };
    let canonical = abs.canonicalize().unwrap_or(abs.clone());

    let meta =
        std::fs::metadata(&canonical).with_context(|| format!("stat {}", canonical.display()))?;
    if !meta.is_file() {
        return Err(anyhow!("{} is not a regular file", canonical.display()));
    }
    let mtime_ns = mtime_ns(&meta);
    let bytes =
        std::fs::read(&canonical).with_context(|| format!("read {}", canonical.display()))?;
    let content_hash = sha256_hex(&bytes);

    let db_path = crate::resolve_db_path(root)?;
    let cached = match session_id.as_deref() {
        Some(sid) => {
            // Ensure the db dir + schema exist (once per process); the bare
            // rusqlite lookup/upsert below need the `session_reads` table.
            ensure_schema(root)?;
            lookup_session_read(&db_path, &canonical, sid)?
        }
        None => None,
    };
    // Content hash is the change signal (mtime alone flips on a no-op touch).
    let unchanged = cached
        .as_ref()
        .map(|c| c.content_hash == content_hash)
        .unwrap_or(false);
    // Baseline = the last full body this (path, session_id) actually saw.
    // Present only after a full/diff read; a stub/entropy-only history has none.
    let baseline = cached.as_ref().and_then(|c| c.content.clone());

    let resolved = match mode {
        ReadMode::Auto if unchanged => ReadMode::Stub,
        // Re-read of an edited file we can diff against the last-seen body.
        ReadMode::Auto | ReadMode::Diff if baseline.is_some() && !unchanged => ReadMode::Diff,
        // No baseline (first read / stub history) or unchanged -> full content.
        ReadMode::Auto | ReadMode::Diff => ReadMode::Full,
        m => m,
    };

    let path_for_storage = canonical.to_string_lossy().into_owned();
    let outline_key = relative_to_root(&canonical, root);

    let payload = match resolved {
        ReadMode::Stub => {
            let syms = outline::outline(store, &outline_key).unwrap_or_default();
            let bytes_returned = serde_json::to_string(&syms)
                .map(|s| s.len())
                .unwrap_or_default();
            if let Some(sid) = session_id.as_deref() {
                upsert_session_read(
                    &db_path,
                    &path_for_storage,
                    sid,
                    mtime_ns,
                    &content_hash,
                    "stub",
                    bytes_returned,
                    None,
                )?;
            }
            json!({
                "path": path_for_storage,
                "mode": "stub",
                "mtime_ns": mtime_ns,
                "content_hash": content_hash,
                "outline": syms,
                "note": "served as outline stub; pass --mode=full for content",
            })
        }
        ReadMode::Full | ReadMode::Auto => {
            let (content, truncated) = truncate_utf8(&bytes, &canonical)?;
            let bytes_returned = content.len();
            if let Some(sid) = session_id.as_deref() {
                upsert_session_read(
                    &db_path,
                    &path_for_storage,
                    sid,
                    mtime_ns,
                    &content_hash,
                    "full",
                    bytes_returned,
                    // Store the served body as the baseline for the next diff.
                    Some(&content),
                )?;
            }
            json!({
                "path": path_for_storage,
                "mode": "full",
                "mtime_ns": mtime_ns,
                "content_hash": content_hash,
                "bytes": bytes_returned,
                "truncated": truncated,
                "content": content,
            })
        }
        ReadMode::Diff => {
            // Resolution guaranteed a baseline + a changed file. Diff the
            // last-seen body against the new one; only serve the diff if it is
            // actually smaller than the whole file (a near-total rewrite is
            // cheaper to send in full), and store the new body as the next
            // baseline either way.
            let (content, truncated) = truncate_utf8(&bytes, &canonical)?;
            let old = baseline.clone().unwrap_or_default();
            let diff_text = unified_diff(&old, &content, &outline_key);
            let serve_diff = !diff_text.is_empty() && diff_text.len() < content.len();
            let served_mode = if serve_diff { "diff" } else { "full" };
            let bytes_returned = if serve_diff {
                diff_text.len()
            } else {
                content.len()
            };
            if let Some(sid) = session_id.as_deref() {
                upsert_session_read(
                    &db_path,
                    &path_for_storage,
                    sid,
                    mtime_ns,
                    &content_hash,
                    served_mode,
                    bytes_returned,
                    Some(&content),
                )?;
            }
            if serve_diff {
                json!({
                    "path": path_for_storage,
                    "mode": "diff",
                    "mtime_ns": mtime_ns,
                    "content_hash": content_hash,
                    "base_content_hash": cached.as_ref().map(|c| c.content_hash.clone()),
                    "bytes": bytes_returned,
                    "truncated": truncated,
                    "diff": diff_text,
                    "note": "unified diff vs the version you last read this session; pass --mode=full for the whole file",
                })
            } else {
                json!({
                    "path": path_for_storage,
                    "mode": "full",
                    "mtime_ns": mtime_ns,
                    "content_hash": content_hash,
                    "bytes": bytes_returned,
                    "truncated": truncated,
                    "content": content,
                    "note": "change too large to diff usefully; served full",
                })
            }
        }
        ReadMode::Entropy => {
            let (content, truncated) = truncate_utf8(&bytes, &canonical)?;
            let mut kept_lines = Vec::new();
            let mut dropped = 0usize;
            for line in content.lines() {
                if shannon_char_entropy(line) >= entropy_threshold {
                    kept_lines.push(line);
                } else {
                    dropped += 1;
                }
            }
            let filtered = kept_lines.join("\n");
            let bytes_returned = filtered.len();
            if let Some(sid) = session_id.as_deref() {
                upsert_session_read(
                    &db_path,
                    &path_for_storage,
                    sid,
                    mtime_ns,
                    &content_hash,
                    "entropy",
                    bytes_returned,
                    None,
                )?;
            }
            json!({
                "path": path_for_storage,
                "mode": "entropy",
                "mtime_ns": mtime_ns,
                "content_hash": content_hash,
                "threshold": entropy_threshold,
                "kept_lines": kept_lines.len(),
                "dropped_lines": dropped,
                "bytes": bytes_returned,
                "truncated": truncated,
                "content": filtered,
            })
        }
    };

    Ok(payload)
}

#[derive(Debug)]
struct CachedRead {
    content_hash: String,
    /// Last full body served to this (path, session_id); `None` when the
    /// only reads so far were stubs/entropy views, so there is nothing to
    /// diff a re-read against.
    content: Option<String>,
}

/// Unified line diff (`old` -> `new`) with 3 lines of context, labelled by
/// the repo-relative path. Empty when the two are identical.
fn unified_diff(old: &str, new: &str, label: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{label}"), &format!("b/{label}"))
        .to_string()
}

fn truncate_utf8(bytes: &[u8], path: &Path) -> Result<(String, bool)> {
    let mut content = match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => return Err(anyhow!("{} is not valid UTF-8", path.display())),
    };
    let truncated = content.len() > MAX_FULL_BYTES;
    if truncated {
        content.truncate(MAX_FULL_BYTES);
        while !content.is_char_boundary(content.len()) {
            content.pop();
        }
    }
    Ok((content, truncated))
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or_default()
}

fn relative_to_root(path: &Path, root: &Path) -> String {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    path.strip_prefix(&canonical_root)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Run `f` with a process-cached connection to the memory db at `db`. The
/// `read` tool's session-read lookup + upsert each ran a fresh
/// `Connection::open` (a few ms apiece on a WAL db), so an agent reading many
/// files paid it twice per read. One cached connection per db (Mutex-guarded
/// for the concurrent serve_http transport) removes the per-call open.
fn with_session_conn<R>(
    db: &Path,
    f: impl FnOnce(&Connection) -> rusqlite::Result<R>,
) -> Result<R> {
    static CONNS: OnceLock<Mutex<HashMap<PathBuf, Connection>>> = OnceLock::new();
    let map = CONNS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map
        .lock()
        .map_err(|_| anyhow!("poisoned session-conn mutex"))?;
    let conn = map.entry(db.to_path_buf()).or_insert_with(|| {
        Connection::open(db)
            .with_context(|| format!("open {}", db.display()))
            .expect("open session conn")
    });
    f(conn).map_err(Into::into)
}

fn lookup_session_read(db: &Path, path: &Path, session_id: &str) -> Result<Option<CachedRead>> {
    with_session_conn(db, |conn| {
        conn.query_row(
            "SELECT content_hash, content FROM session_reads
             WHERE path = ?1 AND session_id = ?2",
            params![path.to_string_lossy(), session_id],
            |r| {
                Ok(CachedRead {
                    content_hash: r.get(0)?,
                    content: r.get(1)?,
                })
            },
        )
        .optional()
    })
}

/// UPSERT the read row. `content` is the full body to keep as the diff
/// baseline: `Some` on full/diff reads (replaces the baseline), `None` on
/// stub/entropy reads (COALESCE preserves whatever full body the caller last
/// saw, so a stub re-read doesn't erase the diff baseline).
#[allow(clippy::too_many_arguments)]
fn upsert_session_read(
    db: &Path,
    path: &str,
    session_id: &str,
    mtime_ns: i64,
    content_hash: &str,
    served_mode: &str,
    bytes_returned: usize,
    content: Option<&str>,
) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    with_session_conn(db, |conn| {
        conn.execute(
            "INSERT INTO session_reads
            (path, session_id, mtime_ns, content_hash, served_mode, served_at, bytes_returned, read_count, content)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)
         ON CONFLICT(path, session_id) DO UPDATE SET
             mtime_ns       = excluded.mtime_ns,
             content_hash   = excluded.content_hash,
             served_mode    = excluded.served_mode,
             served_at      = excluded.served_at,
             bytes_returned = excluded.bytes_returned,
             read_count     = session_reads.read_count + 1,
             content        = COALESCE(excluded.content, session_reads.content)",
            params![path, session_id, mtime_ns, content_hash, served_mode, now, bytes_returned as i64, content],
        )
        .map(|_| ())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_mode_parse_accepts_known_values() {
        assert_eq!(ReadMode::parse("auto").unwrap(), ReadMode::Auto);
        assert_eq!(ReadMode::parse("FULL").unwrap(), ReadMode::Full);
        assert_eq!(ReadMode::parse(" stub ").unwrap(), ReadMode::Stub);
        assert_eq!(ReadMode::parse("entropy").unwrap(), ReadMode::Entropy);
        assert_eq!(ReadMode::parse("Diff").unwrap(), ReadMode::Diff);
    }

    #[test]
    fn unified_diff_empty_for_identical_input() {
        assert!(unified_diff("a\nb\nc\n", "a\nb\nc\n", "x.rs").is_empty());
    }

    #[test]
    fn unified_diff_marks_added_and_removed_lines() {
        let old = "fn a() {}\nfn b() {}\nfn c() {}\n";
        let new = "fn a() {}\nfn b2() {}\nfn c() {}\n";
        let d = unified_diff(old, new, "src/x.rs");
        assert!(d.contains("a/src/x.rs") && d.contains("b/src/x.rs"), "{d}");
        assert!(d.contains("-fn b() {}"), "{d}");
        assert!(d.contains("+fn b2() {}"), "{d}");
        // Unchanged neighbours stay as context, not as +/- churn.
        assert!(d.contains(" fn a() {}"), "{d}");
    }

    #[test]
    fn unified_diff_of_small_edit_is_smaller_than_whole_file() {
        // The core token-savings premise: one changed line in a large file
        // produces a diff far smaller than re-sending the whole file.
        let mut old = String::new();
        for i in 0..400 {
            old.push_str(&format!("line {i} unchanged content here\n"));
        }
        let new = old.replace("line 200 unchanged content here", "line 200 EDITED");
        let d = unified_diff(&old, &new, "big.rs");
        assert!(
            d.len() < new.len() / 4,
            "diff ({} B) should be far smaller than full file ({} B)",
            d.len(),
            new.len()
        );
    }

    #[test]
    fn read_mode_parse_rejects_unknown() {
        assert!(ReadMode::parse("zlib").is_err());
        assert!(ReadMode::parse("").is_err());
    }

    #[test]
    fn shannon_entropy_zero_for_empty_and_constant() {
        assert_eq!(shannon_char_entropy(""), 0.0);
        assert_eq!(shannon_char_entropy("aaaa"), 0.0);
    }

    #[test]
    fn shannon_entropy_high_for_varied_content() {
        assert!((shannon_char_entropy("abab") - 1.0).abs() < 1e-9);
        let line = "fn parse(raw: &str) -> Result<Self, anyhow::Error> {";
        assert!(shannon_char_entropy(line) > 3.5);
    }

    #[test]
    fn shannon_entropy_low_for_repetitive_padding() {
        assert!(shannon_char_entropy("==============================") < 2.5);
        assert!(shannon_char_entropy("                                ") < 2.5);
    }

    /// End-to-end diff-on-re-read lifecycle through `compute`:
    /// full (first read) -> diff (after edit) -> stub (unchanged) -> diff
    /// (after a second edit, vs the *updated* baseline, not the original).
    #[test]
    fn auto_reread_lifecycle_full_then_diff_then_stub_then_diff() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".crabcc")).unwrap();
        let store = Store::open(&root.join(".crabcc").join("index.db")).unwrap();
        let file = root.join("foo.rs");
        let sid = Some("sess-1".to_string());
        let read = |m| compute(root, &store, file.clone(), m, sid.clone(), 0.0).unwrap();

        // A realistically-sized file so a one-line edit diffs smaller than the
        // whole file (a unified diff carries headers + context lines, so on a
        // tiny file the diff is larger and the engine serves full instead).
        let base: String = (0..60).map(|i| format!("fn f{i}() {{ {i} }}\n")).collect();

        std::fs::write(&file, &base).unwrap();
        let r1 = read(ReadMode::Auto);
        assert_eq!(r1["mode"], "full", "first read is full: {r1}");
        assert_eq!(r1["content"], base);

        let v2 = base.replace("fn f30() { 30 }", "fn f30_edited() { 30 }");
        std::fs::write(&file, &v2).unwrap();
        let r2 = read(ReadMode::Auto);
        assert_eq!(r2["mode"], "diff", "re-read after edit is a diff: {r2}");
        let d2 = r2["diff"].as_str().unwrap();
        assert!(
            d2.contains("-fn f30() { 30 }") && d2.contains("+fn f30_edited() { 30 }"),
            "{d2}"
        );
        // The diff must be smaller than the full file (the whole point).
        assert!((r2["bytes"].as_u64().unwrap() as usize) < v2.len(), "{r2}");

        // Unchanged re-read -> stub (and must not erase the v2 baseline).
        let r3 = read(ReadMode::Auto);
        assert_eq!(r3["mode"], "stub", "unchanged re-read is a stub: {r3}");

        let v3 = v2.replace("fn f45() { 45 }", "fn f45_edited() { 45 }");
        std::fs::write(&file, &v3).unwrap();
        let r4 = read(ReadMode::Auto);
        assert_eq!(r4["mode"], "diff", "second edit re-read is a diff: {r4}");
        let d4 = r4["diff"].as_str().unwrap();
        assert!(d4.contains("+fn f45_edited() { 45 }"), "{d4}");
        // Baseline advanced to v2 after r2, so f30_edited is context, not churn.
        assert!(
            !d4.contains("+fn f30_edited"),
            "diff baseline must be v2, not the original: {d4}"
        );
    }

    /// A near-total rewrite produces a diff bigger than the file, so the
    /// engine falls back to serving full content (never larger than `full`).
    #[test]
    fn reread_falls_back_to_full_when_diff_not_smaller() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".crabcc")).unwrap();
        let store = Store::open(&root.join(".crabcc").join("index.db")).unwrap();
        let file = root.join("bar.rs");
        let sid = Some("s".to_string());

        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        compute(root, &store, file.clone(), ReadMode::Auto, sid.clone(), 0.0).unwrap();
        // Replace every line — the diff (all - then all +) exceeds the file.
        std::fs::write(&file, "aaa\nbbb\nccc\nddd\n").unwrap();
        let r = compute(root, &store, file, ReadMode::Auto, sid, 0.0).unwrap();
        assert_eq!(r["mode"], "full", "near-total rewrite serves full: {r}");
    }
}
