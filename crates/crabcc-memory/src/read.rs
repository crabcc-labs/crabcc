//! Outline-stub-aware file read engine. Shared between the CLI's
//! `crabcc read <path>` command and the MCP `crabcc__read` /
//! `crabcc__ctx(tool="read", ...)` tools.
//!
//! Cache key: `(path, session_id)`. Freshness signals: file `mtime_ns`
//! plus `sha256(content)`. Cache hit ⇒ outline stub. Cache miss ⇒
//! full content + UPSERT into `session_reads`.
//!
//! `read_count` increments on every call so the loop detector (#5
//! of the lean-ctx integration plan) sees re-reads.

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
    /// Default — cache check; serve full on miss, stub on hit.
    Auto,
    /// Always serve full content; UPSERTs cache as `full`.
    Full,
    /// Always serve outline stub; UPSERTs cache as `stub`.
    Stub,
    /// Filter lines below an entropy threshold; UPSERTs cache as
    /// `entropy`.
    Entropy,
}

impl ReadMode {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "full" => Ok(Self::Full),
            "stub" => Ok(Self::Stub),
            "entropy" => Ok(Self::Entropy),
            other => Err(anyhow!(
                "--mode must be one of `auto` / `full` / `stub` / `entropy`, got {other:?}"
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
    let is_fresh_hit = cached
        .as_ref()
        .map(|c| c.mtime_ns == mtime_ns && c.content_hash == content_hash)
        .unwrap_or_default();

    let resolved = match mode {
        ReadMode::Auto if is_fresh_hit => ReadMode::Stub,
        ReadMode::Auto => ReadMode::Full,
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
    mtime_ns: i64,
    content_hash: String,
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
    if !map.contains_key(db) {
        let conn = Connection::open(db).with_context(|| format!("open {}", db.display()))?;
        map.insert(db.to_path_buf(), conn);
    }
    let conn = map.get(db).expect("connection just inserted");
    f(conn).map_err(Into::into)
}

fn lookup_session_read(db: &Path, path: &Path, session_id: &str) -> Result<Option<CachedRead>> {
    with_session_conn(db, |conn| {
        conn.query_row(
            "SELECT mtime_ns, content_hash FROM session_reads
             WHERE path = ?1 AND session_id = ?2",
            params![path.to_string_lossy(), session_id],
            |r| {
                Ok(CachedRead {
                    mtime_ns: r.get(0)?,
                    content_hash: r.get(1)?,
                })
            },
        )
        .optional()
    })
}

fn upsert_session_read(
    db: &Path,
    path: &str,
    session_id: &str,
    mtime_ns: i64,
    content_hash: &str,
    served_mode: &str,
    bytes_returned: usize,
) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    with_session_conn(db, |conn| {
        conn.execute(
            "INSERT INTO session_reads
            (path, session_id, mtime_ns, content_hash, served_mode, served_at, bytes_returned, read_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)
         ON CONFLICT(path, session_id) DO UPDATE SET
             mtime_ns       = excluded.mtime_ns,
             content_hash   = excluded.content_hash,
             served_mode    = excluded.served_mode,
             served_at      = excluded.served_at,
             bytes_returned = excluded.bytes_returned,
             read_count     = session_reads.read_count + 1",
            params![path, session_id, mtime_ns, content_hash, served_mode, now, bytes_returned as i64],
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
}
