//! Workspace-aggregate memory search for the `crabcc serve` memory browser.
//!
//! `/api/memory/recent` (see [`crate::memory_view`]) feeds the live "new
//! drawers" column for the *current* repo. This module powers the memory
//! *browser*: a search box that spans **every** repo's drawer store, so one
//! viewer surfaces memory from everywhere an agent has worked.
//!
//! Discovery reuses the same filesystem walk `--workspace` uses for the
//! symbol index — `$CRABCC_HOME/repos/*/` — but keys on `memory.db` instead
//! of `index.db`. Each repo is searched through [`crabcc_memory::Palace`] so
//! the viewer returns the **same ranked hits** `crabcc memory search` does:
//! BM25 (FTS5) + cosine-KNN fused via Reciprocal Rank Fusion. The embedder
//! matches `Palace::open` (`HashEmbedder`) so query and stored vectors agree.
//! If a repo's vector half can't run (e.g. an embedding-dimension mismatch),
//! that repo falls back to lexical BM25 rather than dropping out.

use crate::query::url_decode;
use crate::runtime;
use anyhow::Result;
use crabcc_memory::{DrawerId, HashEmbedder, Palace, SearchMode, SqliteBackend};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const PREVIEW_CHARS: usize = 240;

#[derive(Serialize)]
pub(crate) struct MemorySearchResult {
    /// Echoed back so the client can ignore stale responses from a
    /// fast-typing search box.
    query: String,
    /// Ranking mode actually used: `hybrid` | `lexical` | `vector` | `recent`
    /// (`recent` = empty query, listed newest-first).
    mode: &'static str,
    /// How many repo drawer-stores were searched (1 when `repo=` filters).
    queried_repos: usize,
    /// Every discoverable repo key, regardless of the `repo` filter — drives
    /// the browser's repo dropdown so it always lists the full set.
    repos: Vec<String>,
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
    id: DrawerId,
    /// `source_id` — the cross-call-stable key the detail view fetches by.
    source_id: String,
    wing: String,
    room: Option<String>,
    body_preview: String,
    /// Relevance score (RRF for hybrid, BM25 for lexical, cosine for vector,
    /// 0.0 for the empty-query "recent" listing).
    score: f32,
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

/// Open one repo's drawer store as a [`Palace`]. Mirrors `Palace::open`'s
/// embedder (`HashEmbedder`) so the vector half of hybrid search compares
/// like-for-like against the stored embeddings. `with_components` skips the
/// `repo_root → db_path` resolution because we already hold the db path.
fn open_palace(db: &Path) -> Result<Palace> {
    let backend = SqliteBackend::open(db)?;
    Ok(Palace::with_components(
        Path::new("."),
        Arc::new(backend),
        Arc::new(HashEmbedder::new()),
    ))
}

/// One repo's ranked hits. Empty `query` lists the newest drawers (no
/// ranking, via a read-only recency query). Otherwise runs `mode` through
/// [`Palace`], falling back to lexical BM25 if the requested mode errors
/// (e.g. the vector path can't run for this db).
fn search_one(db: &Path, query: &str, mode: SearchMode, limit: usize) -> Result<Vec<RawHit>> {
    if query.trim().is_empty() {
        return recent_one(db, limit);
    }

    let palace = open_palace(db)?;
    let result = palace
        .search_with_mode(mode, query, limit, None, None)
        .or_else(|_| palace.search_lexical(query, limit, None, None))?;
    let hits = result.hits;

    // DrawerHit omits created_at; batch-fetch it for the date column.
    let ids: Vec<DrawerId> = hits.iter().map(|h| h.id).collect();
    let created: HashMap<DrawerId, i64> = palace
        .backend()
        .get(&ids)
        .map(|g| {
            g.drawers
                .into_iter()
                .map(|d| (d.id, d.created_at))
                .collect()
        })
        .unwrap_or_default();

    Ok(hits
        .into_iter()
        .map(|h| RawHit {
            created_at: created.get(&h.id).copied().unwrap_or(0),
            id: h.id,
            source_id: h.source_id,
            wing: h.wing,
            room: h.room,
            body_preview: preview(&h.body),
            score: h.score,
        })
        .collect())
}

/// Newest-first listing for the empty query. Read-only and Palace-free —
/// `list_drawers` is `id ASC` + SQL-capped (oldest N), the wrong end for a
/// "recent" view. Skips FSST-compressed rows (`body_enc != 0`) just like
/// [`crate::memory_view`]; decoding those needs the `fsst.symbols` sidecar.
fn recent_one(db: &Path, limit: usize) -> Result<Vec<RawHit>> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT d.id, d.source_id, w.name, r.name, substr(d.body, 1, ?2), d.created_at \
         FROM drawers d \
         LEFT JOIN wings w ON w.id = d.wing_id \
         LEFT JOIN rooms r ON r.id = d.room_id \
         WHERE d.body_enc = 0 \
         ORDER BY d.created_at DESC, d.id DESC \
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit as i64, PREVIEW_CHARS as i64], |r| {
        Ok(RawHit {
            id: r.get(0)?,
            source_id: r.get(1)?,
            wing: r.get::<_, Option<String>>(2)?.unwrap_or_else(|| "?".into()),
            room: r.get(3)?,
            body_preview: r.get(4)?,
            score: 0.0,
            created_at: r.get(5)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn preview(body: &str) -> String {
    body.chars().take(PREVIEW_CHARS).collect()
}

struct RawHit {
    id: DrawerId,
    source_id: String,
    wing: String,
    room: Option<String>,
    body_preview: String,
    score: f32,
    created_at: i64,
}

/// `GET /api/memory/search?q=&limit=&repo=&mode=`. Aggregates ranked hits
/// across all repos (or just one when `repo=<key>` is passed). Non-empty
/// queries sort by relevance score; the empty query lists newest drawers.
pub(crate) fn memory_search(root: &Path, query: &str) -> Result<MemorySearchResult> {
    let mut q = String::new();
    let mut limit = DEFAULT_LIMIT;
    let mut repo_filter: Option<String> = None;
    // Honour the user's request to default to hybrid; `mode=` overrides it.
    let mut mode = SearchMode::Hybrid;
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
            "mode" => mode = SearchMode::parse(&v).unwrap_or(SearchMode::Hybrid),
            _ => {}
        }
    }

    // Capture the full repo list before filtering so the dropdown stays
    // complete even when a single repo is selected.
    let all_dbs = discover_memory_dbs(root);
    let repos: Vec<String> = all_dbs.iter().map(|(k, _)| k.clone()).collect();
    let dbs: Vec<(String, PathBuf)> = match &repo_filter {
        Some(key) => all_dbs.into_iter().filter(|(k, _)| k == key).collect(),
        None => all_dbs,
    };

    let mut hits: Vec<SearchHit> = Vec::new();
    for (repo, db) in &dbs {
        // One corrupt/locked db shouldn't sink the whole search.
        match search_one(db, &q, mode, limit) {
            Ok(raw) => {
                for h in raw {
                    hits.push(SearchHit {
                        repo: repo.clone(),
                        id: h.id,
                        source_id: h.source_id,
                        wing: h.wing,
                        room: h.room,
                        body_preview: h.body_preview,
                        score: h.score,
                        created_at: h.created_at,
                    });
                }
            }
            Err(e) => tracing::debug!("memory search: skipping {}: {e:#}", db.display()),
        }
    }

    let is_recent = q.trim().is_empty();
    if is_recent {
        // Newest first across every repo.
        hits.sort_by_key(|h| std::cmp::Reverse(h.created_at));
    } else {
        // Relevance first; per-repo RRF scores aren't perfectly comparable
        // across repos, but score-desc is the right global approximation,
        // with created_at as a stable tie-break.
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.created_at.cmp(&a.created_at))
        });
    }

    let total = hits.len();
    let truncated = total > limit;
    hits.truncate(limit);

    let mode_label = if is_recent {
        "recent"
    } else {
        match mode {
            SearchMode::Hybrid => "hybrid",
            SearchMode::Lexical => "lexical",
            SearchMode::Vector => "vector",
        }
    };

    Ok(MemorySearchResult {
        query: q,
        mode: mode_label,
        queried_repos: dbs.len(),
        repos,
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

    /// Seed a real drawer store at `path` via the same backend + embedder the
    /// viewer uses, so search exercises the genuine BM25/FTS path.
    fn seed(path: &Path, drawers: &[(&str, &str)]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let palace = open_palace(path).unwrap();
        for (source, body) in drawers {
            palace.remember("default", None, source, body).unwrap();
        }
    }

    #[test]
    fn repo_db_path_rejects_traversal() {
        assert!(repo_db_path("../escape").is_none());
        assert!(repo_db_path("a/b").is_none());
        assert!(repo_db_path("").is_none());
        let p = repo_db_path("crabcc-abc123");
        assert!(p
            .map(|p| p.ends_with("repos/crabcc-abc123/memory.db"))
            .unwrap_or(false));
    }

    #[test]
    fn lexical_search_ranks_matching_drawer() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        seed(
            &db,
            &[
                ("doc:1", "the quick brown fox jumps"),
                ("doc:2", "completely unrelated content"),
            ],
        );
        let hits = search_one(&db, "brown fox", SearchMode::Lexical, 10).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].source_id, "doc:1");
        // A non-matching query returns nothing.
        assert!(search_one(&db, "xyzzy", SearchMode::Lexical, 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn empty_query_lists_recent_newest_first() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("memory.db");
        seed(&db, &[("a:1", "first drawer"), ("a:2", "second drawer")]);
        let hits = search_one(&db, "", SearchMode::Hybrid, 10).unwrap();
        assert_eq!(hits.len(), 2);
        // Most-recently inserted sorts first.
        assert_eq!(hits[0].source_id, "a:2");
    }
}
