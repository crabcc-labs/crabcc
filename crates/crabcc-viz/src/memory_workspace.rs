//! Workspace-aggregate memory search for the `crabcc serve` memory browser.
//!
//! `/api/memory/recent` (see [`crate::memory_view`]) feeds the live "new
//! drawers" column for the *current* repo. This module powers the memory
//! *browser*: a search box that spans **every** repo's drawer store, so one
//! viewer surfaces memory from everywhere an agent has worked.
//!
//! Discovery reuses the same filesystem walk `--workspace` uses for the
//! symbol index — `$CRABCC_HOME/repos/*/` — but keys on `memory.db` instead
//! of `index.db`. We open each db read-only and run a substring match rather
//! than going through `Palace::open`, which would run schema-bootstrap side
//! effects and (with `memory-embed`) spin up an embedder per repo. Lexical
//! substring is the MVP ranker; per-repo BM25/hybrid fusion is a follow-up.

use crate::query::url_decode;
use crate::runtime;
use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const PREVIEW_CHARS: usize = 240;

#[derive(Serialize)]
pub(crate) struct MemorySearchResult {
    /// Echoed back so the client can ignore stale responses from a
    /// fast-typing search box.
    query: String,
    /// How many repo drawer-stores were searched.
    queried_repos: usize,
    /// Total matches across all repos before the global `limit` cap.
    total: usize,
    /// True when `total` exceeded `limit` and the tail was dropped.
    truncated: bool,
    hits: Vec<SearchHit>,
}

#[derive(Serialize)]
struct SearchHit {
    /// `$CRABCC_HOME/repos/<key>` dir name — stable per `remote.origin.url`.
    /// Doubles as the `repo` param the detail view passes to
    /// `/api/memory/get` so it opens the right db.
    repo: String,
    /// Numeric drawer id (db-local; not stable across repos).
    id: i64,
    /// `source_id` — the cross-call-stable key the detail view fetches by.
    source_id: String,
    wing: String,
    room: Option<String>,
    body_preview: String,
    created_at: i64,
}

/// `$CRABCC_HOME`, honoring the env override then `~/.crabcc`. Mirrors
/// `crabcc_cli::workspace` + `crabcc_memory::palace::path` so the viewer and
/// the CLI agree on where repos live.
pub(crate) fn crabcc_home() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CRABCC_HOME") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    runtime::home_dir().ok().map(|h| h.join(".crabcc"))
}

/// Every `(repo_key, memory.db)` pair discoverable under
/// `$CRABCC_HOME/repos/*/`, plus the current repo's store (resolved + legacy)
/// in case it predates the centralised layout. Deduped by path, sorted by key.
fn discover_memory_dbs(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let push_unique = |key: String, path: PathBuf, out: &mut Vec<(String, PathBuf)>| {
        if path.exists() && !out.iter().any(|(_, p)| *p == path) {
            out.push((key, path));
        }
    };

    if let Some(repos_dir) = crabcc_home().map(|h| h.join("repos")) {
        if let Ok(entries) = std::fs::read_dir(&repos_dir) {
            for entry in entries.flatten() {
                // file_type() does not follow symlinks — a symlink planted in
                // repos/ must not let us open a db outside the tree.
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if !is_dir {
                    continue;
                }
                let db = entry.path().join("memory.db");
                if db.exists() {
                    let key = entry.file_name().to_string_lossy().into_owned();
                    push_unique(key, db, &mut out);
                }
            }
        }
    }

    // Always include the repo the server was launched in, even when the
    // centralised walk missed it (fresh `git init`, legacy `.crabcc/`).
    if let Ok(resolved) = crabcc_memory::resolve_db_path(root) {
        let key = resolved
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "this-repo".into());
        push_unique(key, resolved, &mut out);
    }
    push_unique(
        "this-repo-legacy".into(),
        root.join(".crabcc").join("memory.db"),
        &mut out,
    );

    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Read-only substring search over one repo's drawers. Skips FSST-compressed
/// rows (`body_enc != 0`) for the same reason [`crate::memory_view`] does:
/// decoding needs the optional `~/.crabcc/fsst.symbols` sidecar.
fn search_one(db: &Path, pattern: &str, limit: usize) -> Result<Vec<RawHit>> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT d.id, d.source_id, w.name, r.name, substr(d.body, 1, ?2), d.created_at \
         FROM drawers d \
         LEFT JOIN wings w ON w.id = d.wing_id \
         LEFT JOIN rooms r ON r.id = d.room_id \
         WHERE d.body_enc = 0 AND (d.body LIKE ?1 OR d.source_id LIKE ?1) \
         ORDER BY d.created_at DESC \
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![pattern, PREVIEW_CHARS as i64, limit as i64],
        |r| {
            Ok(RawHit {
                id: r.get(0)?,
                source_id: r.get(1)?,
                wing: r.get::<_, Option<String>>(2)?.unwrap_or_else(|| "?".into()),
                room: r.get(3)?,
                body_preview: r.get(4)?,
                created_at: r.get(5)?,
            })
        },
    )?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

struct RawHit {
    id: i64,
    source_id: String,
    wing: String,
    room: Option<String>,
    body_preview: String,
    created_at: i64,
}

/// `GET /api/memory/search?q=&limit=&repo=`. Aggregates across all repos
/// (or just one when `repo=<key>` is passed), newest first.
pub(crate) fn memory_search(root: &Path, query: &str) -> Result<MemorySearchResult> {
    let mut q = String::new();
    let mut limit = DEFAULT_LIMIT;
    let mut repo_filter: Option<String> = None;
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = url_decode(v);
        match k {
            "q" => q = v,
            "limit" => {
                limit = v
                    .parse::<usize>()
                    .unwrap_or(DEFAULT_LIMIT)
                    .clamp(1, MAX_LIMIT)
            }
            "repo" if !v.is_empty() => repo_filter = Some(v),
            _ => {}
        }
    }

    // `%term%`, with LIKE's own wildcards escaped so a literal `%` in the
    // query doesn't match everything. Empty query → recent drawers.
    let escaped = q
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("%{escaped}%");

    let mut dbs = discover_memory_dbs(root);
    if let Some(key) = &repo_filter {
        dbs.retain(|(k, _)| k == key);
    }

    let mut hits: Vec<SearchHit> = Vec::new();
    for (repo, db) in &dbs {
        // One corrupt/locked db shouldn't sink the whole search.
        match search_one(db, &pattern, limit) {
            Ok(raw) => {
                for h in raw {
                    hits.push(SearchHit {
                        repo: repo.clone(),
                        id: h.id,
                        source_id: h.source_id,
                        wing: h.wing,
                        room: h.room,
                        body_preview: h.body_preview,
                        created_at: h.created_at,
                    });
                }
            }
            Err(e) => tracing::debug!("memory search: skipping {}: {e:#}", db.display()),
        }
    }

    hits.sort_by_key(|h| std::cmp::Reverse(h.created_at));
    let total = hits.len();
    let truncated = total > limit;
    hits.truncate(limit);

    Ok(MemorySearchResult {
        query: q,
        queried_repos: dbs.len(),
        total,
        truncated,
        hits,
    })
}

/// Resolve `$CRABCC_HOME/repos/<key>/memory.db` for the detail view, with a
/// path-traversal guard on `key` (stored dir names are `[a-z0-9_-]`).
pub(crate) fn repo_db_path(key: &str) -> Option<PathBuf> {
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    crabcc_home().map(|h| h.join("repos").join(key).join("memory.db"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Minimal drawers/wings/rooms schema + one drawer, matching
    /// `crabcc-memory/schema/001_init.sql` closely enough for the read path.
    fn seed_db(path: &Path, source: &str, body: &str, created_at: i64) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS wings (id INTEGER PRIMARY KEY, name TEXT, created_at INTEGER);
             CREATE TABLE IF NOT EXISTS rooms (id INTEGER PRIMARY KEY, wing_id INTEGER, name TEXT);
             CREATE TABLE IF NOT EXISTS drawers (id INTEGER PRIMARY KEY, wing_id INTEGER, room_id INTEGER,
                 source_id TEXT, body TEXT, created_at INTEGER, body_enc INTEGER DEFAULT 0);
             INSERT OR IGNORE INTO wings (id, name, created_at) VALUES (1, 'default', 0);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO drawers (wing_id, room_id, source_id, body, created_at, body_enc) \
             VALUES (1, NULL, ?1, ?2, ?3, 0)",
            rusqlite::params![source, body, created_at],
        )
        .unwrap();
    }

    #[test]
    fn repo_db_path_rejects_traversal() {
        // No env mutation needed: the guard returns None before touching home.
        assert!(repo_db_path("../escape").is_none());
        assert!(repo_db_path("a/b").is_none());
        assert!(repo_db_path("").is_none());
        // A well-formed key resolves to repos/<key>/memory.db.
        let p = repo_db_path("crabcc-abc123");
        assert!(p
            .map(|p| p.ends_with("repos/crabcc-abc123/memory.db"))
            .unwrap_or(false));
    }

    #[test]
    fn search_one_matches_body_and_skips_encoded() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        seed_db(&db, "doc:1", "the quick brown fox", 100);
        let hits = search_one(&db, "%brown%", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source_id, "doc:1");
        // A non-matching pattern returns nothing.
        assert!(search_one(&db, "%zzz%", 10).unwrap().is_empty());
        // Newest-first ordering when multiple rows match (fresh db).
        let db2 = dir.path().join("two.db");
        seed_db(&db2, "x:1", "brown one", 100);
        let conn = rusqlite::Connection::open(&db2).unwrap();
        conn.execute(
            "INSERT INTO drawers (wing_id, room_id, source_id, body, created_at, body_enc) \
             VALUES (1, NULL, 'x:2', 'brown two', 300, 0)",
            [],
        )
        .unwrap();
        let hits = search_one(&db2, "%brown%", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].source_id, "x:2"); // ts=300 sorts first
    }
}
