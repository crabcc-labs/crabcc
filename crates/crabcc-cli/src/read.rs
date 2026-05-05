//! `crabcc read <path>` — outline-stub-aware file read.
//!
//! Per the lean-ctx integration plan (#3), AI agents repeatedly read
//! the same files in a session. The first read hands back full
//! content; subsequent reads on the same `(path, session_id)` get an
//! outline-shaped JSON stub instead — slashing token cost when the
//! caller already has the body cached.
//!
//! Cache key: `(path, session_id)`. Freshness signals: file mtime
//! (ns) and a SHA-256 of the content. Cache hit ⇒ stub. Cache miss
//! (no row, mtime newer, or hash mismatch) ⇒ full content + cache
//! UPSERT. The `read_count` column is bumped on every call to feed
//! the loop detector (#5 of the plan).
//!
//! Storage: `session_reads` table in `$CRABCC_HOME/repos/<slug>-<hash6>/memory.db`,
//! resolved via `crabcc_memory::resolve_db_path`. The Palace's
//! schema-init step lands the table on first open.

use anyhow::{anyhow, Context, Result};
use crabcc_core::{hash::sha256_hex, outline, store::Store};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum bytes returned in `--mode=full`. Keeps a single `crabcc
/// read` from blowing past a token-budget cliff on a 50 MB log file.
/// 256 KiB is roughly 64 k tokens — generous for source files,
/// trips on assets / generated bundles / test fixtures.
const MAX_FULL_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadMode {
    /// Default — cache check; serve full on miss, stub on hit.
    Auto,
    /// Always serve full content; UPSERTs cache as `full`.
    Full,
    /// Always serve outline stub; UPSERTs cache as `stub`.
    Stub,
}

impl ReadMode {
    fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "full" => Ok(Self::Full),
            "stub" => Ok(Self::Stub),
            other => Err(anyhow!(
                "--mode must be one of `auto` / `full` / `stub`, got {other:?}"
            )),
        }
    }

    fn served_label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Full => "full",
            Self::Stub => "stub",
        }
    }
}

/// Run `crabcc read`. `root` is the repo root (the same path passed
/// to other commands). `path` may be relative (resolved against
/// `root`) or absolute.
pub fn run(
    root: &Path,
    store: &Store,
    path: PathBuf,
    mode_raw: &str,
    session_id_arg: Option<String>,
) -> Result<()> {
    let mode = ReadMode::parse(mode_raw)?;
    let session_id = effective_session_id(session_id_arg);

    let abs = if path.is_absolute() {
        path.clone()
    } else {
        root.join(&path)
    };
    let canonical = abs.canonicalize().unwrap_or(abs.clone());

    // Stat once; mtime + content hash are the freshness gate.
    let meta =
        std::fs::metadata(&canonical).with_context(|| format!("stat {}", canonical.display()))?;
    if !meta.is_file() {
        return Err(anyhow!("{} is not a regular file", canonical.display()));
    }
    let mtime_ns = mtime_ns(&meta);
    let bytes =
        std::fs::read(&canonical).with_context(|| format!("read {}", canonical.display()))?;
    let content_hash = sha256_hex(&bytes);

    // Cache lookup runs only when we have a session id. No session
    // id ⇒ caching is opt-in via env or flag, so an unset caller
    // always gets fresh full content. When a session id IS set, we
    // make sure the memory.db file (and schema) exist before any
    // rusqlite call — `Palace::open` is idempotent and handles
    // schema migration + dir creation.
    let db_path = crabcc_memory::resolve_db_path(root)?;
    let cached = match session_id.as_deref() {
        Some(sid) => {
            let _ = crabcc_memory::Palace::open(root)?;
            lookup_session_read(&db_path, &canonical, sid)?
        }
        None => None,
    };
    let is_fresh_hit = cached
        .as_ref()
        .map(|c| c.mtime_ns == mtime_ns && c.content_hash == content_hash)
        .unwrap_or(false);

    let serve_stub = match mode {
        ReadMode::Stub => true,
        ReadMode::Full => false,
        ReadMode::Auto => is_fresh_hit,
    };

    let path_for_storage = canonical.to_string_lossy().to_string();
    let outline_key = relative_to_root(&canonical, root);

    let payload = if serve_stub {
        // Outline lookup off the existing index. If the file isn't
        // in the index (e.g. a Markdown doc that crabcc skips), the
        // outline list is empty — still useful to the caller as a
        // signal that the cache hit was real, just without symbol
        // shape.
        let syms = outline::outline(store, &outline_key).unwrap_or_default();
        let bytes_returned = sonic_rs::to_string(&syms).map(|s| s.len()).unwrap_or(0);
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
    } else {
        // Truncate to MAX_FULL_BYTES so a single read can't blow
        // the agent's context. Mark the truncation so the agent
        // knows to ask for a follow-up window.
        let mut content = match std::str::from_utf8(&bytes) {
            Ok(s) => s.to_string(),
            Err(_) => return Err(anyhow!("{} is not valid UTF-8", canonical.display())),
        };
        let truncated = content.len() > MAX_FULL_BYTES;
        if truncated {
            content.truncate(MAX_FULL_BYTES);
            // Truncate at a char boundary if we landed in the middle
            // of a multi-byte sequence.
            while !content.is_char_boundary(content.len()) {
                content.pop();
            }
        }
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
    };

    crabcc_core::track::record(
        "read",
        &path_for_storage,
        1,
        &repo_label(root),
        payload.to_string().len(),
    );
    let _ = mode.served_label(); // placeholder for future telemetry

    println!("{payload}");
    Ok(())
}

#[derive(Debug)]
struct CachedRead {
    mtime_ns: i64,
    content_hash: String,
}

fn effective_session_id(arg: Option<String>) -> Option<String> {
    if let Some(id) = arg {
        if !id.trim().is_empty() {
            return Some(id);
        }
    }
    std::env::var("CRABCC_SESSION_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn relative_to_root(path: &Path, root: &Path) -> String {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    path.strip_prefix(&canonical_root)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

fn repo_label(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root.to_string_lossy().to_string())
}

fn lookup_session_read(db: &Path, path: &Path, session_id: &str) -> Result<Option<CachedRead>> {
    let conn = Connection::open(db).with_context(|| format!("open {}", db.display()))?;
    let row = conn
        .query_row(
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
        .optional()?;
    Ok(row)
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
        .unwrap_or(0);
    let conn = Connection::open(db).with_context(|| format!("open {}", db.display()))?;
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
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_mode_parse_accepts_known_values() {
        assert_eq!(ReadMode::parse("auto").unwrap(), ReadMode::Auto);
        assert_eq!(ReadMode::parse("FULL").unwrap(), ReadMode::Full);
        assert_eq!(ReadMode::parse(" stub ").unwrap(), ReadMode::Stub);
    }

    #[test]
    fn read_mode_parse_rejects_unknown() {
        assert!(ReadMode::parse("entropy").is_err());
        assert!(ReadMode::parse("").is_err());
    }

    #[test]
    fn effective_session_id_prefers_flag_over_env() {
        std::env::set_var("CRABCC_SESSION_ID", "env-id");
        let got = effective_session_id(Some("flag-id".to_string()));
        std::env::remove_var("CRABCC_SESSION_ID");
        assert_eq!(got.as_deref(), Some("flag-id"));
    }

    #[test]
    fn effective_session_id_falls_back_to_env() {
        std::env::set_var("CRABCC_SESSION_ID", "env-id");
        let got = effective_session_id(None);
        std::env::remove_var("CRABCC_SESSION_ID");
        assert_eq!(got.as_deref(), Some("env-id"));
    }

    #[test]
    fn effective_session_id_treats_blank_as_none() {
        std::env::remove_var("CRABCC_SESSION_ID");
        assert!(effective_session_id(Some("   ".to_string())).is_none());
        assert!(effective_session_id(None).is_none());
    }
}
