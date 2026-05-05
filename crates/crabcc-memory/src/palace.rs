//! Top-level memory facade + multi-project routing.
//!
//! `Palace::open(repo_root)` is idempotent: creates
//! `<repo_root>/.crabcc/memory.db` if missing, reuses if present. Per-repo
//! by design — the memory store is scoped to one git working tree, the
//! same way `.crabcc/index.db` is for the symbol index.
//!
//! `PalaceRegistry` caches open palaces by canonical git root. Lets one
//! long-running process (notably the MCP server) handle tool calls from
//! multiple projects: each call carries a `cwd` arg, the registry walks
//! up to find `.git`, and returns (or opens) the matching palace.
//!
//! `find_git_root(start)` is the standalone walk-up helper — public so
//! callers building tool args can resolve the route deterministically
//! before invoking the registry.

use crate::backend::{sqlite::SqliteBackend, Backend, LexicalQuery};
use crate::embed::{Embedder, HashEmbedder};
use crate::types::*;
use anyhow::{Context, Result};
use crabcc_core::hash::sha256_hex;
use moka::sync::Cache;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

/// Reciprocal Rank Fusion constant (Cormack/Clarke/Buettcher 2009). Larger
/// `k` softens the rank curve so top-of-list disagreements between rankers
/// matter less; the original paper used 60 and that's also what TREC and
/// most production hybrid-search stacks default to.
const RRF_K: usize = 60;

/// Default upper bound on simultaneously cached open palaces. Each palace
/// holds a SQLite connection; 64 leaves headroom for long-running MCP
/// sessions that bounce between many `cwd` values without bloating memory.
/// Override via `CRABCC_PALACE_CACHE_CAPACITY`.
pub const DEFAULT_PALACE_CACHE_CAPACITY: u64 = 64;

/// Time-to-idle for the palace cache: a palace untouched for this long is
/// evicted, dropping its `Arc` so the SQLite connection can close. Tuned
/// to outlast a typical flurry of MCP tool calls but reclaim memory
/// during long idle stretches.
pub const PALACE_CACHE_TTI: Duration = Duration::from_secs(10 * 60);

/// TTL on the `find_git_root` memo cache. Short enough that worktree
/// creations / removals reflect within human-perceptible time, long
/// enough that bursts of sequential MCP calls amortize the walk.
pub const GIT_ROOT_CACHE_TTL: Duration = Duration::from_secs(60);

/// Max distinct paths memoized for the git-root walk-up. Cheap entries
/// (`PathBuf` + `Option<PathBuf>`), so 256 covers normal multi-project
/// use without measurable memory pressure.
pub const GIT_ROOT_CACHE_CAPACITY: u64 = 256;

/// Which retriever(s) to use.
///
/// The `Default` impl is feature-conditional (issue #20):
///
/// - With `memory-embed` ON → `Hybrid` (vector + lexical via RRF).
///   Real semantic embeddings make the vector path informative.
/// - With `memory-embed` OFF → `Lexical` (BM25 only).
///   The default `HashEmbedder` produces deterministic-but-meaningless
///   vectors — running cosine over them is noise, so we fall back to
///   keyword search until a real embedder is plugged in.
///
/// `Vector` is exposed for ablation regardless of features, and remains
/// what callers will explicitly pick once they've wired their own
/// `Embedder` impl through `Palace::with_components`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Hybrid,
    Lexical,
    Vector,
}

// Feature-conditional default — clippy's `derivable_impls` would have us
// `#[derive(Default)]` with `#[default]` on a single variant, but the
// chosen variant flips with a cargo feature so a hand-written impl is
// the only way to express that.
#[allow(clippy::derivable_impls)]
impl Default for SearchMode {
    fn default() -> Self {
        // With no real embedder available, vector hits are meaningless,
        // so the safe default is BM25-only. Hosts that construct
        // `Palace::with_components(..., Arc<FastEmbedder>)` can still
        // ask for `Hybrid` explicitly.
        #[cfg(feature = "memory-embed")]
        {
            Self::Hybrid
        }
        #[cfg(not(feature = "memory-embed"))]
        {
            Self::Lexical
        }
    }
}

impl SearchMode {
    /// Parse the CLI / API value. Accepts the three lower-case spellings
    /// plus a couple of common aliases.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "hybrid" | "rrf" | "fusion" => Some(Self::Hybrid),
            "lexical" | "fts" | "bm25" | "text" => Some(Self::Lexical),
            "vector" | "knn" | "ann" | "embed" => Some(Self::Vector),
            _ => None,
        }
    }
}

pub struct Palace {
    pub root: PathBuf,
    backend: Arc<dyn Backend>,
    embedder: Arc<dyn Embedder>,
}

impl Palace {
    /// Open or create a persistent palace. As of #479 (May 2026) the
    /// db lives at `$CRABCC_HOME/repos/<slug>-<hash6>/memory.db` —
    /// see [`resolve_db_path`] for the layout. Idempotent: reuses the
    /// existing db if present, copies a legacy
    /// `<repo_root>/.crabcc/memory.db` over on first open.
    /// Default embedder is `HashEmbedder` until M1 wires `fastembed-rs`.
    pub fn open(repo_root: &Path) -> Result<Self> {
        let db_path = resolve_db_path(repo_root)?;
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        migrate_legacy_if_needed(repo_root, &db_path)?;
        let backend = SqliteBackend::open(&db_path)?;
        Ok(Self {
            root: repo_root.to_path_buf(),
            backend: Arc::new(backend),
            embedder: Arc::new(HashEmbedder::new()),
        })
    }

    /// Ephemeral palace — in-memory backend, no persistence. For tests
    /// and one-shot operations.
    pub fn ephemeral() -> Self {
        Self {
            root: PathBuf::from("."),
            backend: Arc::new(crate::backend::in_memory::InMemoryBackend::new()),
            embedder: Arc::new(HashEmbedder::new()),
        }
    }

    pub fn with_components(
        repo_root: &Path,
        backend: Arc<dyn Backend>,
        embedder: Arc<dyn Embedder>,
    ) -> Self {
        Self {
            root: repo_root.to_path_buf(),
            backend,
            embedder,
        }
    }

    pub fn backend(&self) -> &dyn Backend {
        &*self.backend
    }

    pub fn embedder(&self) -> &dyn Embedder {
        &*self.embedder
    }

    /// Embed and store one drawer. Returns its id.
    pub fn remember(
        &self,
        wing: &str,
        room: Option<&str>,
        source_id: &str,
        body: &str,
    ) -> Result<DrawerId> {
        self.remember_in_session(wing, room, source_id, body, None)
    }

    /// Same as `remember`, with an explicit session id. Used by auto-capture
    /// hooks that pass through `$TERM_SESSION_ID` or an MCP-supplied id.
    ///
    /// When the `markdown` feature is on (issue #54), the body is run
    /// through [`crabcc_core::md::sanitize_drawer_body`] before storage
    /// — strips code-fence backticks, heading markers, list bullets,
    /// link/image URLs etc. so BM25 / FTS5 ranks on the textual
    /// content rather than markdown syntax artefacts. The embedding
    /// is computed against the *sanitized* body too, so vector hits
    /// match the keyword path. With the feature off, the body is
    /// stored verbatim (default).
    pub fn remember_in_session(
        &self,
        wing: &str,
        room: Option<&str>,
        source_id: &str,
        body: &str,
        session_id: Option<&str>,
    ) -> Result<DrawerId> {
        #[cfg(feature = "markdown")]
        let normalised: String = crabcc_core::md::sanitize_drawer_body(body);
        #[cfg(feature = "markdown")]
        let body_for_storage: &str = &normalised;
        #[cfg(not(feature = "markdown"))]
        let body_for_storage: &str = body;

        let emb = self.embedder.embed_one(body_for_storage)?;
        let ids = self.backend.add(&[DrawerInsert {
            wing: wing.into(),
            room: room.map(str::to_string),
            source_id: source_id.into(),
            body: body_for_storage.into(),
            embedding: emb,
            session_id: session_id.map(str::to_string),
        }])?;
        ids.into_iter().next().context("backend returned no id")
    }

    /// Default search — hybrid (vector + lexical + RRF). M1 flipped this
    /// from vector-only because LongMemEval R@5 jumps ~5pts on the
    /// MemPalace fixture once both rankers vote. Use `search_with_mode`
    /// for ablation.
    pub fn search(&self, query: &str, limit: usize) -> Result<QueryResult> {
        self.search_filtered(query, limit, None, None)
    }

    /// Hybrid search with optional wing/room filters.
    pub fn search_filtered(
        &self,
        query: &str,
        limit: usize,
        wing: Option<&str>,
        room: Option<&str>,
    ) -> Result<QueryResult> {
        self.search_with_mode(SearchMode::default(), query, limit, wing, room)
    }

    /// Vector-only search — embeds the query and returns the top-K cosine
    /// neighbours. Equivalent to the pre-M1 `search()` behaviour.
    pub fn search_vector(
        &self,
        query: &str,
        limit: usize,
        wing: Option<&str>,
        room: Option<&str>,
    ) -> Result<QueryResult> {
        let emb = self.embedder.embed_one(query)?;
        self.backend.query(&Query {
            embedding: emb,
            limit,
            wing: wing.map(str::to_string),
            room: room.map(str::to_string),
        })
    }

    /// Lexical-only search — BM25 on FTS5 (SQLite backend) or token-overlap
    /// (in-memory backend). No embedding cost, useful for keyword queries
    /// where you want the exact-token match to dominate.
    pub fn search_lexical(
        &self,
        query: &str,
        limit: usize,
        wing: Option<&str>,
        room: Option<&str>,
    ) -> Result<QueryResult> {
        self.backend.query_lexical(&LexicalQuery {
            text: query.to_string(),
            limit,
            wing: wing.map(str::to_string),
            room: room.map(str::to_string),
        })
    }

    /// Hybrid search via Reciprocal Rank Fusion (k = 60). Issues both the
    /// vector and lexical queries, blends by RRF, and returns the top-K
    /// fused hits. Each ranker is asked for `2 * limit` candidates so a
    /// short ranker doesn't starve fusion at the long-tail.
    pub fn search_hybrid(
        &self,
        query: &str,
        limit: usize,
        wing: Option<&str>,
        room: Option<&str>,
    ) -> Result<QueryResult> {
        let pool = limit.saturating_mul(2).max(limit);
        let vector_hits = self.search_vector(query, pool, wing, room)?.hits;
        let lexical_hits = self.search_lexical(query, pool, wing, room)?.hits;
        Ok(QueryResult {
            hits: rrf_fuse(&[&vector_hits, &lexical_hits], limit),
        })
    }

    /// Dispatch on `SearchMode`. CLI / MCP entry point.
    pub fn search_with_mode(
        &self,
        mode: SearchMode,
        query: &str,
        limit: usize,
        wing: Option<&str>,
        room: Option<&str>,
    ) -> Result<QueryResult> {
        match mode {
            SearchMode::Hybrid => self.search_hybrid(query, limit, wing, room),
            SearchMode::Lexical => self.search_lexical(query, limit, wing, room),
            SearchMode::Vector => self.search_vector(query, limit, wing, room),
        }
    }

    /// Enumerate drawers; thin pass-through to `Backend::list_drawers`.
    pub fn list_drawers(&self, wing: Option<&str>, limit: usize) -> Result<Vec<Drawer>> {
        self.backend.list_drawers(wing, limit)
    }

    /// Fetch one drawer verbatim by id.
    pub fn get(&self, id: DrawerId) -> Result<Option<Drawer>> {
        Ok(self.backend.get(&[id])?.drawers.into_iter().next())
    }

    /// Delete drawers by selector. Returns rows removed.
    pub fn delete(&self, sel: &DeleteSel) -> Result<usize> {
        self.backend.delete(sel)
    }

    /// `forget` is `delete` plus a SQLite `VACUUM` afterward — rows
    /// disappear AND the on-disk file shrinks. Used by `crabcc memory
    /// forget` (issue #26) where reclaiming space is part of the
    /// contract; for transient delete-then-reinsert flows the plain
    /// `delete` is cheaper because VACUUM rewrites the entire file.
    ///
    /// Idempotent on missing IDs: an `Empty` selector or one that
    /// matches no rows still runs successfully (returning 0 rows
    /// removed) and still triggers VACUUM. Backends that don't support
    /// VACUUM (the in-memory one) treat this as a plain `delete`.
    pub fn forget(&self, sel: &DeleteSel) -> Result<usize> {
        let n = self.backend.delete(sel)?;
        self.backend.vacuum()?;
        Ok(n)
    }

    /// Drawer count.
    pub fn count(&self) -> Result<usize> {
        self.backend.count()
    }

    /// Health snapshot.
    pub fn health(&self) -> HealthStatus {
        self.backend.health()
    }

    /// Walk a directory and ingest one drawer per text file. Thin wrapper
    /// over [`crate::mine::project::mine_project`] kept on `Palace` so
    /// callers stay on the recommended facade.
    pub fn mine_project(
        &self,
        path: &Path,
        opts: &crate::mine::project::MineProjectOpts,
    ) -> Result<crate::mine::MineReport> {
        crate::mine::project::mine_project(self, path, opts)
    }

    /// Walk a directory of Claude Code JSONL transcripts and ingest one
    /// drawer per `(user, assistant)` turn pair. See
    /// [`crate::mine::sessions::mine_sessions`].
    pub fn mine_sessions(
        &self,
        dir: &Path,
        opts: &crate::mine::sessions::MineSessionsOpts,
    ) -> Result<crate::mine::MineReport> {
        crate::mine::sessions::mine_sessions(self, dir, opts)
    }
}

/// Walk up from `start` looking for `.git/`. Returns the first ancestor
/// containing one, or `None` if not in a git repo.
///
/// Hot paths (the MCP tool dispatch loop) should prefer routing through
/// Resolve the on-disk path for a repo's `memory.db`. Layout:
///
/// ```text
/// $CRABCC_HOME/repos/<slug>-<hash6>/memory.db
/// ```
///
/// * `$CRABCC_HOME` defaults to `$HOME/.crabcc` (matches the convention
///   already used by `crabcc-cli/backup.rs`, `model_info`, agents/, etc.).
/// * `<slug>` is the repo dir's basename, ascii-lowered, with non-
///   `[a-z0-9_-]` collapsed to `-`. Falls back to `unknown-repo` when
///   the path has no terminal component.
/// * `<hash6>` is the first 6 hex chars of `sha256(origin_url)`. When
///   no `remote.origin.url` is set (fresh `git init`, non-git dir), the
///   slug stands alone — at the cost of basename collisions across
///   unrelated repos sharing a name.
///
/// Centralising memory.db in `$CRABCC_HOME` means worktrees of the same
/// repo share one db (they share `.git/config`), and `git clean -fdx`
/// in the working tree doesn't blow it away.
pub fn resolve_db_path(repo_root: &Path) -> Result<PathBuf> {
    let home = crabcc_home_dir()?;
    let key = repo_storage_key(repo_root);
    Ok(home.join("repos").join(key).join("memory.db"))
}

fn crabcc_home_dir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CRABCC_HOME") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME").context("$HOME is not set")?;
    Ok(PathBuf::from(home).join(".crabcc"))
}

fn repo_storage_key(repo_root: &Path) -> String {
    let raw_basename = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown-repo".into());
    let slug = sanitize_slug(&raw_basename);
    match git_origin_url(repo_root) {
        Some(url) => {
            let hash6: String = sha256_hex(url.as_bytes()).chars().take(6).collect();
            format!("{slug}-{hash6}")
        }
        None => slug,
    }
}

fn sanitize_slug(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| {
            let lower = c.to_ascii_lowercase();
            if lower.is_ascii_alphanumeric() || lower == '-' || lower == '_' {
                lower
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = s.trim_matches('-');
    if trimmed.is_empty() {
        "unknown-repo".to_string()
    } else {
        trimmed.to_string()
    }
}

fn git_origin_url(repo_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// One-shot migration: if `<repo_root>/.crabcc/memory.db` exists and
/// the new global path doesn't, copy it over so the user's existing
/// drawers carry forward. Idempotent — once the new path exists,
/// subsequent opens skip the check.
fn migrate_legacy_if_needed(repo_root: &Path, new_path: &Path) -> Result<()> {
    let legacy = repo_root.join(".crabcc").join("memory.db");
    if !legacy.exists() || new_path.exists() {
        return Ok(());
    }
    if let Some(parent) = new_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::copy(&legacy, new_path)
        .with_context(|| format!("copy {} -> {}", legacy.display(), new_path.display()))?;
    eprintln!(
        "crabcc-memory: migrated drawers from {} -> {}",
        legacy.display(),
        new_path.display()
    );
    Ok(())
}

/// [`PalaceRegistry::open_for`] / [`PalaceRegistry::resolve_git_root`],
/// which memoize this walk for [`GIT_ROOT_CACHE_TTL`] so a flurry of
/// calls with the same `cwd` only pays the canonicalize + ancestor scan
/// once per minute.
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut p = start.canonicalize().ok()?;
    loop {
        if p.join(".git").exists() {
            return Some(p);
        }
        p = p.parent()?.to_path_buf();
    }
}

/// Cache of open palaces, keyed by canonical repo root.
///
/// The MCP server uses this to handle tool calls from multiple projects in
/// one process: each call carries a `cwd` arg, the registry walks up to
/// find the git root, and returns (or opens) the matching palace.
///
/// Backed by `moka::sync::Cache` for bounded memory and time-to-idle
/// eviction — a palace untouched for [`PALACE_CACHE_TTI`] is dropped,
/// which lets its SQLite connection close once any in-flight callers
/// release their `Arc`. Capacity bound is
/// [`DEFAULT_PALACE_CACHE_CAPACITY`] (overridable via
/// `CRABCC_PALACE_CACHE_CAPACITY`).
///
/// Also memoizes the `find_git_root` walk for [`GIT_ROOT_CACHE_TTL`] so
/// repeated MCP calls from the same `cwd` don't re-walk to find `.git`.
pub struct PalaceRegistry {
    palaces: Cache<PathBuf, Arc<Palace>>,
    git_roots: Cache<PathBuf, Option<PathBuf>>,
}

impl PalaceRegistry {
    pub fn new() -> Self {
        let capacity = std::env::var("CRABCC_PALACE_CACHE_CAPACITY")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_PALACE_CACHE_CAPACITY);
        Self::with_capacity(capacity)
    }

    /// Build a registry with an explicit max capacity. Useful in tests
    /// that want to exercise eviction without overriding env vars.
    pub fn with_capacity(capacity: u64) -> Self {
        let palaces = Cache::builder()
            .max_capacity(capacity)
            .time_to_idle(PALACE_CACHE_TTI)
            // moka invokes the eviction listener on every removal with
            // the cache's last strong ref. Explicitly dropping it here
            // releases that ref; any callers still holding an Arc keep
            // the palace alive, and the underlying SQLite Connection
            // closes as soon as the last Arc drops.
            .eviction_listener(|_key, value: Arc<Palace>, _cause| {
                drop(value);
            })
            .build();
        let git_roots = Cache::builder()
            .max_capacity(GIT_ROOT_CACHE_CAPACITY)
            .time_to_live(GIT_ROOT_CACHE_TTL)
            .build();
        Self { palaces, git_roots }
    }

    /// Build a registry tuned for tests — short TTI/TTL so eviction can
    /// be observed within a unit-test wall-time budget.
    #[doc(hidden)]
    pub fn with_test_timings(capacity: u64, palace_tti: Duration, git_root_ttl: Duration) -> Self {
        let palaces = Cache::builder()
            .max_capacity(capacity)
            .time_to_idle(palace_tti)
            .eviction_listener(|_key, value: Arc<Palace>, _cause| {
                drop(value);
            })
            .build();
        let git_roots = Cache::builder()
            .max_capacity(GIT_ROOT_CACHE_CAPACITY)
            .time_to_live(git_root_ttl)
            .build();
        Self { palaces, git_roots }
    }

    /// Resolve the git root for `start`, memoizing the result. Cache
    /// hits skip both the canonicalize syscall and the up-walk.
    pub fn resolve_git_root(&self, start: &Path) -> Option<PathBuf> {
        let key = start.to_path_buf();
        self.git_roots.get_with(key, || find_git_root(start))
    }

    /// Look up the palace for `cwd_or_root`. If `cwd_or_root` isn't itself
    /// a git root, walks up to find one. Falls back to using the path as-is
    /// if no `.git` is found upstream.
    ///
    /// Concurrent calls for the same key coalesce inside moka — only one
    /// thread runs the loader, the rest get the cached `Arc` once it's
    /// inserted. This preserves the prior `Mutex<HashMap>` invariant
    /// of "all racers see the same Arc".
    pub fn open_for(&self, cwd_or_root: &Path) -> Result<Arc<Palace>> {
        let root = self
            .resolve_git_root(cwd_or_root)
            .unwrap_or_else(|| cwd_or_root.to_path_buf());
        let canon = root.canonicalize().unwrap_or(root.clone());
        // try_get_with coalesces concurrent loads on the same key. moka
        // wraps the loader's error in Arc since try_get_with requires
        // its error type to be Clone; unwrap that wrapper into a fresh
        // anyhow at the boundary.
        self.palaces
            .try_get_with(canon.clone(), || Palace::open(&canon).map(Arc::new))
            .map_err(|e: Arc<anyhow::Error>| anyhow::anyhow!("open palace: {e}"))
    }

    /// Live-entry count in the palace cache. Drives moka's
    /// pending-eviction pass first so the count reflects steady state —
    /// without that, recent inserts can briefly exceed `max_capacity`
    /// until the maintenance thread catches up.
    pub fn count(&self) -> usize {
        self.palaces.run_pending_tasks();
        self.palaces.entry_count() as usize
    }

    /// Force eviction of any expired entries. Public so benches and
    /// tests can sample the steady-state cache size deterministically.
    #[doc(hidden)]
    pub fn run_pending_tasks(&self) {
        self.palaces.run_pending_tasks();
        self.git_roots.run_pending_tasks();
    }

    /// Number of memoized `find_git_root` entries currently live.
    /// Hidden from public docs; used by tests to assert TTL invalidation.
    #[doc(hidden)]
    pub fn git_root_cache_count(&self) -> usize {
        self.git_roots.run_pending_tasks();
        self.git_roots.entry_count() as usize
    }
}

impl Default for PalaceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Reciprocal Rank Fusion across N rankers. Each `ranking` is a list of
/// hits in descending-quality order; the contribution of a hit at rank `r`
/// (1-based) to the fused score is `1 / (RRF_K + r)`. Hits that appear in
/// more than one ranking accumulate score, which is how the "vote across
/// rankers" intuition emerges from the math. Output is sorted by fused
/// score descending and truncated to `limit`. The first ranker breaks ties
/// among rankings so the order is deterministic.
fn rrf_fuse(rankings: &[&[DrawerHit]], limit: usize) -> Vec<DrawerHit> {
    if limit == 0 || rankings.iter().all(|r| r.is_empty()) {
        return Vec::new();
    }
    let mut fused: HashMap<DrawerId, (f32, DrawerHit)> = HashMap::new();
    for ranking in rankings {
        for (rank, hit) in ranking.iter().enumerate() {
            let contribution = 1.0_f32 / (RRF_K as f32 + (rank + 1) as f32);
            fused
                .entry(hit.id)
                .and_modify(|(s, _)| *s += contribution)
                .or_insert((contribution, hit.clone()));
        }
    }
    let mut out: Vec<(f32, DrawerHit)> = fused.into_values().collect();
    // Stable order: fused score desc, then drawer id asc on ties.
    out.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.id.cmp(&b.1.id))
    });
    out.truncate(limit);
    out.into_iter()
        .map(|(score, mut hit)| {
            // Surface the fused RRF score on the returned hit so callers
            // can see why hybrid ordered the list this way. Single-ranker
            // raw scores stay accessible via `search_vector` /
            // `search_lexical` directly.
            hit.score = score;
            hit
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Pin every test in this file to the same `$CRABCC_HOME`
    /// tempdir, so `Palace::open(repo_root)` resolves to a private
    /// `repos/<slug>-<hash6>/memory.db` underneath it instead of
    /// stomping the user's real `~/.crabcc/`. We never re-set the env
    /// var after init — the path is stable for the test process — so
    /// tests don't need any per-test mutex.
    ///
    /// The leaked `TempDir` keeps the directory alive for the
    /// process lifetime. cargo's test runner cleans up `$TMPDIR`
    /// itself, so the leak is contained.
    fn ensure_test_crabcc_home() {
        static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        let path = HOME.get_or_init(|| {
            let d = tempfile::tempdir().expect("test crabcc-home tempdir");
            let path = d.path().to_path_buf();
            std::mem::forget(d);
            path
        });
        // Re-assert the env var on every call. Other test modules
        // (notably crabcc-cli's backup tests) mutate `CRABCC_HOME`
        // mid-run; without this re-pin, we'd race onto whatever they
        // set.
        std::env::set_var("CRABCC_HOME", path);
    }

    #[test]
    fn open_creates_db_under_crabcc_home() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        let expected = resolve_db_path(dir.path()).unwrap();
        assert!(
            expected.exists(),
            "expected {} to exist",
            expected.display()
        );
        assert_eq!(p.root, dir.path().to_path_buf());
    }

    #[test]
    fn migrate_legacy_db_on_first_open() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        // Pre-stage a real, opened-then-closed legacy SQLite file via
        // SqliteBackend itself. Writing arbitrary bytes wouldn't pass
        // the SQLite header check on the migrated copy, so we use the
        // real backend to guarantee a valid file.
        let legacy_dir = dir.path().join(".crabcc");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_db = legacy_dir.join("memory.db");
        {
            let _ = SqliteBackend::open(&legacy_db).unwrap();
        }

        let new_path = resolve_db_path(dir.path()).unwrap();
        // Safety: a previous test run may have left a slugged subdir
        // behind (basename collisions across tempdirs). Clear so the
        // migration branch in `Palace::open` fires.
        if new_path.exists() {
            std::fs::remove_file(&new_path).unwrap();
        }
        let _p = Palace::open(dir.path()).unwrap();

        // The migrated file must be byte-identical to the legacy one.
        let legacy_bytes = std::fs::read(&legacy_db).unwrap();
        let migrated_bytes = std::fs::read(&new_path).unwrap();
        assert_eq!(migrated_bytes, legacy_bytes);
    }

    #[test]
    fn slug_sanitizer_collapses_special_chars() {
        assert_eq!(sanitize_slug("My Cool Repo!"), "my-cool-repo");
        assert_eq!(sanitize_slug("foo_bar-1"), "foo_bar-1");
        assert_eq!(sanitize_slug("///"), "unknown-repo");
        assert_eq!(sanitize_slug(""), "unknown-repo");
    }

    #[test]
    fn repo_storage_key_falls_back_to_slug_without_origin() {
        // tempdir → no .git, so no origin URL. Slug-only key.
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let key = repo_storage_key(dir.path());
        assert!(!key.is_empty());
        assert!(!key.contains('/'));
        // No hash suffix when origin is missing — slug stands alone.
        assert!(
            !key.contains('-')
                || key.matches('-').count()
                    == sanitize_slug(&dir.path().file_name().unwrap().to_string_lossy())
                        .matches('-')
                        .count(),
            "key {key:?} should not have an extra `-<hash6>` suffix"
        );
    }

    #[test]
    fn open_is_idempotent_and_reuses_data() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let id1 = {
            let p = Palace::open(dir.path()).unwrap();
            p.remember("default", None, "doc:1", "the fox jumps")
                .unwrap()
        };
        let id2 = {
            let p = Palace::open(dir.path()).unwrap();
            // Re-opening must see the prior drawer (dedup returns same id).
            p.remember("default", None, "doc:1", "the fox jumps")
                .unwrap()
        };
        assert_eq!(id1, id2, "second open must reuse persisted drawer");
    }

    #[test]
    fn remember_then_search() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        p.remember("default", None, "1", "the fox jumps").unwrap();
        p.remember("default", None, "2", "the cat sleeps").unwrap();
        let r = p.search("fox jumps", 5).unwrap();
        assert!(!r.hits.is_empty());
        assert_eq!(r.hits[0].source_id, "1");
    }

    #[test]
    fn ephemeral_does_not_persist() {
        let p1 = Palace::ephemeral();
        p1.remember("d", None, "x", "hello").unwrap();
        let p2 = Palace::ephemeral();
        assert_eq!(p2.backend().count().unwrap(), 0);
    }

    #[test]
    fn find_git_root_walks_up() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let nested = dir.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        let found = find_git_root(&nested).unwrap();
        assert_eq!(found, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn registry_caches_per_root() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let nested = dir.path().join("sub/sub2");
        std::fs::create_dir_all(&nested).unwrap();

        let reg = PalaceRegistry::new();
        let p1 = reg.open_for(&nested).unwrap();
        let p2 = reg.open_for(dir.path()).unwrap();
        // Both lookups resolve to the same git root → same Arc.
        assert!(Arc::ptr_eq(&p1, &p2));
        assert_eq!(reg.count(), 1);
    }

    #[test]
    fn registry_separates_distinct_roots() {
        ensure_test_crabcc_home();
        let a = tempdir().unwrap();
        std::fs::create_dir_all(a.path().join(".git")).unwrap();
        let b = tempdir().unwrap();
        std::fs::create_dir_all(b.path().join(".git")).unwrap();

        let reg = PalaceRegistry::new();
        let pa = reg.open_for(a.path()).unwrap();
        let pb = reg.open_for(b.path()).unwrap();
        assert!(!Arc::ptr_eq(&pa, &pb));
        assert_eq!(reg.count(), 2);
    }

    #[test]
    fn palace_search_with_no_drawers_is_empty() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        assert!(p.search("anything", 5).unwrap().hits.is_empty());
    }

    #[test]
    fn registry_concurrent_open_for_same_root() {
        // 4 threads race to open_for the same git root. Registry must
        // hand out the same Arc to all of them and end with count == 1.
        use std::thread;
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let reg = Arc::new(PalaceRegistry::new());
        let mut handles = Vec::new();
        for _ in 0..4 {
            let r = reg.clone();
            let p = dir.path().to_path_buf();
            handles.push(thread::spawn(move || r.open_for(&p).unwrap()));
        }
        let palaces: Vec<Arc<Palace>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let first = palaces[0].clone();
        for p in &palaces[1..] {
            assert!(Arc::ptr_eq(&first, p), "all threads must get same Arc");
        }
        assert_eq!(reg.count(), 1);
    }

    #[test]
    fn find_git_root_returns_none_outside_repo() {
        // tempdir lives under /tmp or /var/folders — neither is inside a git
        // repo on a normal system, so walk-up should exhaust to None.
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let result = find_git_root(dir.path());
        assert!(
            result.is_none(),
            "tempdir unexpectedly inside a git repo at {result:?}"
        );
    }

    #[test]
    fn ephemeral_palaces_are_independent() {
        let p1 = Palace::ephemeral();
        let p2 = Palace::ephemeral();
        p1.remember("d", None, "x", "hello").unwrap();
        assert_eq!(p1.backend().count().unwrap(), 1);
        assert_eq!(p2.backend().count().unwrap(), 0);
    }

    #[test]
    fn remember_in_session_round_trips_session_id() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        let id = p
            .remember_in_session("default", None, "doc:1", "hello", Some("s1"))
            .unwrap();
        let d = p.get(id).unwrap().expect("drawer present");
        assert_eq!(d.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn remember_without_session_yields_no_session_id() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        let id = p.remember("default", None, "doc:1", "hello").unwrap();
        let d = p.get(id).unwrap().expect("drawer present");
        assert!(d.session_id.is_none());
    }

    #[test]
    fn remember_in_session_persists_across_reopen() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        {
            let p = Palace::open(dir.path()).unwrap();
            p.remember_in_session("default", None, "doc:1", "hello", Some("durable"))
                .unwrap();
        }
        {
            let p = Palace::open(dir.path()).unwrap();
            let drawers = p.list_drawers(None, 10).unwrap();
            assert_eq!(drawers[0].session_id.as_deref(), Some("durable"));
        }
    }

    #[test]
    fn search_filtered_returns_session_carrying_drawer() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        p.remember_in_session("default", None, "doc:1", "fox jumps", Some("s1"))
            .unwrap();
        let r = p.search("fox jumps", 1).unwrap();
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].source_id, "doc:1");
        // Round-trip via get to confirm session_id is populated on the row.
        let d = p.get(r.hits[0].id).unwrap().unwrap();
        assert_eq!(d.session_id.as_deref(), Some("s1"));
    }

    // ---------- M1: search modes + RRF fusion ----------

    fn mk_hit(id: i64, score: f32) -> DrawerHit {
        DrawerHit {
            id,
            score,
            source_id: format!("src:{id}"),
            body: format!("body-{id}"),
            wing: "default".into(),
            room: None,
        }
    }

    #[test]
    fn rrf_single_ranker_preserves_order() {
        let v = vec![mk_hit(1, 0.9), mk_hit(2, 0.8), mk_hit(3, 0.7)];
        let fused = rrf_fuse(&[&v], 5);
        assert_eq!(
            fused.iter().map(|h| h.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        // RRF score for rank-1 = 1/(60+1) = 0.01639...; rank-2 < rank-1.
        assert!(fused[0].score > fused[1].score);
        assert!(fused[1].score > fused[2].score);
    }

    #[test]
    fn rrf_combines_two_rankers() {
        // A appears at rank 1 in vector + rank 3 in lexical → strong fused.
        // B appears at rank 2 in vector only.
        // C appears at rank 1 in lexical only.
        // Expected fused order: A > C > B (A's two contributions outweigh
        // either single ranker's top hit).
        let vector = [mk_hit(1, 0.99), mk_hit(2, 0.5), mk_hit(99, 0.1)];
        let lexical = [mk_hit(3, 9.0), mk_hit(4, 2.0), mk_hit(1, 1.0)];
        let fused = rrf_fuse(&[&vector, &lexical], 10);
        // A (id=1) must lead: 1/(60+1) + 1/(60+3) = 0.0164 + 0.0159 = 0.0323
        // C (id=3) is 1/(60+1) = 0.0164
        // B (id=2) is 1/(60+2) = 0.0161
        assert_eq!(fused[0].id, 1, "A should win after fusion");
        assert_eq!(fused[1].id, 3, "C is next (lexical rank 1)");
        assert_eq!(fused[2].id, 2, "B follows (vector rank 2 only)");
        assert_eq!(fused[3].id, 4, "D (lexical rank 2 only)");
        assert_eq!(fused[4].id, 99, "E (vector rank 3 only) last");
    }

    #[test]
    fn rrf_empty_inputs_yield_empty() {
        assert!(rrf_fuse(&[], 5).is_empty());
        let empty: &[DrawerHit] = &[];
        assert!(rrf_fuse(&[empty, empty], 5).is_empty());
    }

    #[test]
    fn rrf_limit_zero_yields_empty() {
        let v = [mk_hit(1, 0.9)];
        assert!(rrf_fuse(&[&v], 0).is_empty());
    }

    #[test]
    fn rrf_truncates_to_limit() {
        let v: Vec<DrawerHit> = (1..=10).map(|i| mk_hit(i, 1.0 / i as f32)).collect();
        let fused = rrf_fuse(&[&v], 3);
        assert_eq!(fused.len(), 3);
        assert_eq!(
            fused.iter().map(|h| h.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn rrf_ties_break_by_id_ascending() {
        // Two rankings that produce identical fused scores for two ids —
        // the deterministic tie-breaker must yield the lower id first.
        let a = [mk_hit(7, 0.5), mk_hit(3, 0.4)];
        let b = [mk_hit(3, 0.5), mk_hit(7, 0.4)];
        // Both 3 and 7 get 1/(60+1) + 1/(60+2) = same score. Order: 3 < 7.
        let fused = rrf_fuse(&[&a, &b], 5);
        assert_eq!(fused[0].id, 3);
        assert_eq!(fused[1].id, 7);
    }

    #[test]
    fn search_mode_parse_aliases() {
        assert_eq!(SearchMode::parse("hybrid"), Some(SearchMode::Hybrid));
        assert_eq!(SearchMode::parse("RRF"), Some(SearchMode::Hybrid));
        assert_eq!(SearchMode::parse("lexical"), Some(SearchMode::Lexical));
        assert_eq!(SearchMode::parse("BM25"), Some(SearchMode::Lexical));
        assert_eq!(SearchMode::parse("FTS"), Some(SearchMode::Lexical));
        assert_eq!(SearchMode::parse("vector"), Some(SearchMode::Vector));
        assert_eq!(SearchMode::parse("KNN"), Some(SearchMode::Vector));
        assert_eq!(SearchMode::parse("nonsense"), None);
    }

    #[test]
    fn search_mode_default_matches_feature_gate() {
        // Default flips with the `memory-embed` feature (issue #20):
        // real embedder available → Hybrid; otherwise → Lexical so we
        // don't blend meaningless HashEmbedder cosine into the result.
        #[cfg(feature = "memory-embed")]
        assert_eq!(SearchMode::default(), SearchMode::Hybrid);
        #[cfg(not(feature = "memory-embed"))]
        assert_eq!(SearchMode::default(), SearchMode::Lexical);
    }

    #[test]
    fn search_lexical_finds_keyword_match() {
        // BM25 path on a SQLite palace: drawers whose body contains the
        // queried token must surface; unrelated drawers must not.
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        p.remember("default", None, "doc:1", "the quick brown fox")
            .unwrap();
        p.remember("default", None, "doc:2", "lazy cat sleeps in the sun")
            .unwrap();
        p.remember("default", None, "doc:3", "fox in the henhouse")
            .unwrap();
        let r = p
            .search_with_mode(SearchMode::Lexical, "fox", 5, None, None)
            .unwrap();
        let ids: std::collections::HashSet<&str> =
            r.hits.iter().map(|h| h.source_id.as_str()).collect();
        assert!(ids.contains("doc:1"));
        assert!(ids.contains("doc:3"));
        assert!(!ids.contains("doc:2"));
    }

    #[test]
    fn search_vector_returns_self_first() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        p.remember("default", None, "doc:1", "alpha beta gamma")
            .unwrap();
        p.remember("default", None, "doc:2", "completely unrelated")
            .unwrap();
        let r = p
            .search_with_mode(SearchMode::Vector, "alpha beta gamma", 5, None, None)
            .unwrap();
        assert_eq!(r.hits[0].source_id, "doc:1");
    }

    #[test]
    fn search_hybrid_fuses_and_returns_results() {
        // Smoke test: hybrid must find the queried term even when the stub
        // embedder's vector contribution is noise — RRF fusion lets the
        // BM25 ranker carry the win for keyword queries.
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        p.remember("default", None, "doc:1", "quantum entanglement experiment")
            .unwrap();
        p.remember("default", None, "doc:2", "kitchen recipe for risotto")
            .unwrap();
        p.remember("default", None, "doc:3", "quantum theory introduction")
            .unwrap();
        let r = p.search("quantum", 5).unwrap();
        let ids: std::collections::HashSet<&str> =
            r.hits.iter().map(|h| h.source_id.as_str()).collect();
        // Both "quantum" docs surface; the unrelated risotto doc may or
        // may not depending on vector scoring (HashEmbedder noise) — we
        // only assert the keyword ones are present.
        assert!(ids.contains("doc:1"));
        assert!(ids.contains("doc:3"));
    }

    #[test]
    fn search_hybrid_score_is_rrf_score() {
        // Hybrid hits must carry the fused RRF score, not the raw cosine
        // or BM25 value. RRF rank-1 score is bounded by 2/(60+1) when both
        // rankers agree on top-1 — well below 1.0.
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        p.remember("default", None, "doc:1", "exact phrase match here")
            .unwrap();
        p.remember("default", None, "doc:2", "different content")
            .unwrap();
        let r = p.search("exact phrase match here", 5).unwrap();
        assert!(!r.hits.is_empty());
        // RRF fused score is at most 2/(RRF_K + 1) when both rankers list
        // the hit at rank 1.
        let max_possible = 2.0_f32 / (RRF_K as f32 + 1.0);
        assert!(
            r.hits[0].score <= max_possible + 1e-6,
            "score {} > max RRF {}",
            r.hits[0].score,
            max_possible
        );
    }

    #[test]
    fn search_default_surfaces_lexical_signal() {
        // A regression guard: if someone re-flips `Palace::search` to
        // pure vector by accident, this test fires. Asserts that the
        // BM25 winner appears under the default `search()` — which is
        // true whether the default is `Hybrid` (RRF includes lexical)
        // or `Lexical` (BM25-only).
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        // Two docs with WILDLY different lexical content. If the default
        // were vector-only, the ordering would be embedder-noise, not
        // BM25-meaningful.
        p.remember("default", None, "doc:1", "needle haystack token")
            .unwrap();
        p.remember("default", None, "doc:2", "completely orthogonal text")
            .unwrap();
        let r = p.search("needle", 2).unwrap();
        let ids: Vec<&str> = r.hits.iter().map(|h| h.source_id.as_str()).collect();
        assert!(ids.contains(&"doc:1"));
    }

    #[test]
    fn search_lexical_persists_across_reopen() {
        // FTS5 index must survive close/reopen. Write on first open,
        // close, reopen, lexical-search on second open.
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        {
            let p = Palace::open(dir.path()).unwrap();
            p.remember("default", None, "doc:1", "persistent body")
                .unwrap();
        }
        let p = Palace::open(dir.path()).unwrap();
        let r = p
            .search_with_mode(SearchMode::Lexical, "persistent", 5, None, None)
            .unwrap();
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].source_id, "doc:1");
    }

    #[test]
    fn search_lexical_room_filter() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        p.remember("default", Some("kitchen"), "doc:1", "tomato basil pasta")
            .unwrap();
        p.remember("default", Some("garage"), "doc:2", "tomato seedlings")
            .unwrap();
        let r = p
            .search_with_mode(SearchMode::Lexical, "tomato", 10, None, Some("kitchen"))
            .unwrap();
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].source_id, "doc:1");
    }

    // ─── moka cache integration (issue #30) ───────────────────────────

    #[test]
    fn registry_evicts_palace_after_tti() {
        // Tight TTI so the test resolves in <1s. Each open_for call
        // refreshes the idle timer; sleeping past it without further
        // access must trigger eviction on the next maintenance pass.
        use std::thread::sleep;
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let reg = PalaceRegistry::with_test_timings(
            8,
            Duration::from_millis(80),
            Duration::from_secs(60),
        );

        let _p = reg.open_for(dir.path()).unwrap();
        assert_eq!(reg.count(), 1, "freshly opened palace must be cached");

        sleep(Duration::from_millis(150));
        // count() runs pending tasks, which is what actually evicts
        // expired entries in moka's lazy maintenance model.
        assert_eq!(
            reg.count(),
            0,
            "palace must be evicted once idle for longer than TTI"
        );
    }

    #[test]
    fn registry_capacity_bound_evicts_oldest() {
        // capacity = 2; opening a third palace must drop one of the
        // earlier two so the cache stays at most two entries.
        ensure_test_crabcc_home();
        let a = tempdir().unwrap();
        std::fs::create_dir_all(a.path().join(".git")).unwrap();
        let b = tempdir().unwrap();
        std::fs::create_dir_all(b.path().join(".git")).unwrap();
        let c = tempdir().unwrap();
        std::fs::create_dir_all(c.path().join(".git")).unwrap();

        let reg =
            PalaceRegistry::with_test_timings(2, Duration::from_secs(60), Duration::from_secs(60));
        reg.open_for(a.path()).unwrap();
        reg.open_for(b.path()).unwrap();
        reg.open_for(c.path()).unwrap();
        reg.run_pending_tasks();
        assert!(
            reg.count() <= 2,
            "cache must not exceed max_capacity, got {}",
            reg.count()
        );
    }

    #[test]
    fn git_root_memo_invalidates_after_ttl() {
        // 50ms TTL — first lookup populates, second within window is a
        // hit, sleeping past the TTL drops the entry.
        use std::thread::sleep;
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let reg = PalaceRegistry::with_test_timings(
            8,
            Duration::from_secs(60),
            Duration::from_millis(50),
        );

        let canonical = dir.path().canonicalize().unwrap();
        assert_eq!(reg.resolve_git_root(dir.path()).unwrap(), canonical);
        assert_eq!(
            reg.git_root_cache_count(),
            1,
            "first lookup must populate the memo"
        );
        // Second call inside the TTL window — still cached.
        let _ = reg.resolve_git_root(dir.path());
        assert_eq!(reg.git_root_cache_count(), 1);

        sleep(Duration::from_millis(100));
        // run_pending_tasks() drives moka's lazy expiration so the
        // count reflects the post-TTL state.
        assert_eq!(
            reg.git_root_cache_count(),
            0,
            "memo must drop the entry once TTL elapses"
        );
    }

    #[test]
    fn git_root_memo_skips_walk_on_hit() {
        // Same `cwd` looked up twice in quick succession — second call
        // must return the cached value even after we delete the .git
        // marker on disk. (Outside the TTL window the marker delete
        // would propagate; inside it the memo wins.)
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let reg =
            PalaceRegistry::with_test_timings(8, Duration::from_secs(60), Duration::from_secs(60));

        let first = reg.resolve_git_root(dir.path()).unwrap();
        std::fs::remove_dir_all(dir.path().join(".git")).unwrap();
        // Without the memo, removing .git would now make the walk
        // return None for this `start`; with the memo the prior Some
        // wins.
        let second = reg
            .resolve_git_root(dir.path())
            .expect("memoized result must survive .git removal within TTL");
        assert_eq!(first, second);
    }

    // ─── recall@5 golden test (issue #20) ──────────────────────────────
    //
    // 50 short canned lines covering 10 thematic clusters of 5 docs each;
    // 7 representative queries each with a known-good source_id. Asserts
    // recall@5 ≥ 0.8 — i.e., the right doc is in the top-5 for at least
    // 6 of the 7 queries — under the default search mode (Lexical
    // without `memory-embed`, Hybrid with). Documents that the lexical
    // fallback is informative on its own; running this same test with
    // `--features memory-embed` should hit recall@5 = 1.0 once a real
    // embedder is plugged in.

    fn golden_drawers() -> &'static [(&'static str, &'static str)] {
        &[
            // cluster 1: HTTP servers
            (
                "http:1",
                "configure listen address and port for the HTTP server",
            ),
            (
                "http:2",
                "rate limiting middleware for inbound HTTP requests",
            ),
            (
                "http:3",
                "TLS termination and certificate auto-renewal via ACME",
            ),
            ("http:4", "CORS headers for cross-origin browser requests"),
            ("http:5", "websocket upgrade handshake on the HTTP listener"),
            // cluster 2: SQL / databases
            ("db:1", "create a new postgres database with utf8 encoding"),
            ("db:2", "alter table to add a non-null default column"),
            ("db:3", "transaction isolation levels for sqlite WAL mode"),
            (
                "db:4",
                "rollback strategy for a failed batch insert into postgres",
            ),
            (
                "db:5",
                "connection pooling for high-throughput SQL workloads",
            ),
            // cluster 3: tree-sitter / parsing
            ("ts:1", "tree-sitter walks symbol tables in source code"),
            ("ts:2", "ast-grep patterns for matching function call sites"),
            ("ts:3", "incremental reparse on edit using tree-sitter"),
            (
                "ts:4",
                "node kind dispatch table for typescript and javascript",
            ),
            ("ts:5", "tree-sitter-rust grammar handles impl blocks"),
            // cluster 4: embeddings / vectors
            ("vec:1", "MiniLM-L6-v2 produces 384-dim semantic embeddings"),
            (
                "vec:2",
                "cosine similarity over L2 normalized float vectors",
            ),
            (
                "vec:3",
                "sqlite-vec virtual table for ANN nearest neighbor search",
            ),
            ("vec:4", "fastembed batch embed reuses the ort session"),
            (
                "vec:5",
                "FNV xorshift fallback for deterministic test embeddings",
            ),
            // cluster 5: filesystem / IO
            ("fs:1", "ignore-aware repository walk respecting gitignore"),
            (
                "fs:2",
                "memchr fast prefilter for byte needle in source files",
            ),
            ("fs:3", "mmap large index database read only access"),
            (
                "fs:4",
                "filesystem watcher debouncing rapid file save bursts",
            ),
            (
                "fs:5",
                "atomic write to temp file then rename for durability",
            ),
            // cluster 6: compression
            (
                "zip:1",
                "FSST symbol table compresses repeated string prefixes",
            ),
            (
                "zip:2",
                "zstd level tradeoff between size and decompression speed",
            ),
            ("zip:3", "decode FSST encoded rows lazily on read"),
            (
                "zip:4",
                "bench compression ratio across natural language and code",
            ),
            (
                "zip:5",
                "cargo feature gate for optional fsst-rs dependency",
            ),
            // cluster 7: caching
            (
                "cache:1",
                "moka time to idle eviction closes idle SQLite connections",
            ),
            (
                "cache:2",
                "sha256 keyed cache for repeated embedding queries",
            ),
            ("cache:3", "git root memoization with a 60 second TTL"),
            (
                "cache:4",
                "bounded LRU palace registry keyed by canonical repo root",
            ),
            (
                "cache:5",
                "FSST codec arc shared across store sessions no contention",
            ),
            // cluster 8: testing
            (
                "test:1",
                "tempfile based integration test cleans up in drop",
            ),
            (
                "test:2",
                "ignore attribute for network dependent end to end checks",
            ),
            ("test:3", "snapshot comparison for golden file fixtures"),
            ("test:4", "criterion benchmark harness reports throughput"),
            (
                "test:5",
                "table driven tests with parameterized search queries",
            ),
            // cluster 9: tokens / shaping
            ("tok:1", "tokens per query saved versus raw grep walk"),
            (
                "tok:2",
                "limit flag short circuits per file walk early stop",
            ),
            (
                "tok:3",
                "files only output dedupes hits across the same file",
            ),
            (
                "tok:4",
                "count mode emits only an integer not the matched lines",
            ),
            (
                "tok:5",
                "JSON projection via jq pipeline trims irrelevant fields",
            ),
            // cluster 10: misc / off-topic
            (
                "misc:1",
                "release pipeline builds linux mac windows binaries",
            ),
            (
                "misc:2",
                "homebrew tap distributes prebuilt mac arm64 binaries",
            ),
            (
                "misc:3",
                "starship custom module renders crabcc status in the prompt",
            ),
            (
                "misc:4",
                "claude code skill auto routes lookups to crabcc CLI",
            ),
            ("misc:5", "MCP JSON RPC dispatches tool calls over stdio"),
        ]
    }

    fn golden_queries() -> &'static [(&'static str, &'static str)] {
        &[
            ("ACME TLS", "http:3"),
            ("postgres database utf8", "db:1"),
            ("ast-grep call sites", "ts:2"),
            ("MiniLM 384 dim", "vec:1"),
            ("memchr byte needle", "fs:2"),
            ("fsst symbol table", "zip:1"),
            ("moka idle SQLite", "cache:1"),
        ]
    }

    #[test]
    fn search_recall_at_5_meets_threshold() {
        // Verify clause from issue #20: recall@5 ≥ 0.8 over the canned
        // corpus. Runs under the *default* search mode for the build
        // (Lexical when `memory-embed` is off; Hybrid when on).
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        for (id, body) in golden_drawers() {
            p.remember("default", None, id, body)
                .unwrap_or_else(|e| panic!("remember {id}: {e}"));
        }

        let queries = golden_queries();
        let mut hits = 0usize;
        for (query, expected) in queries {
            let r = p.search(query, 5).unwrap();
            let ids: Vec<&str> = r.hits.iter().map(|h| h.source_id.as_str()).collect();
            if ids.iter().any(|id| id == expected) {
                hits += 1;
            } else {
                eprintln!("miss: query={query:?} expected={expected:?} top5={ids:?}");
            }
        }

        let recall = hits as f64 / queries.len() as f64;
        assert!(
            recall >= 0.8,
            "recall@5 = {recall:.2} (got {hits}/{}); issue #20 requires ≥ 0.80",
            queries.len()
        );
    }

    /// Issue #22 deliverable — contrived semantic-distractor set proving
    /// the value of RRF over either single ranker:
    ///
    /// 1. **Vec wins on semantic** — a paraphrased query with no token
    ///    overlap with the target body finds the right drawer under
    ///    `Vector`, but `Lexical` (BM25) misses entirely.
    /// 2. **Lex wins on literal** — a rare exact-token query finds the
    ///    target under `Lexical`, but `Vector` (cosine on MiniLM) ranks
    ///    a topically-similar distractor above the literal-token drawer.
    /// 3. **Hybrid wins on both** — RRF fusion ranks the right drawer at
    ///    position 1 for *both* query types, so the user doesn't need to
    ///    pick a mode per query.
    ///
    /// Gated on `memory-embed` because real semantic similarity needs
    /// `FastEmbedder` (MiniLM-L6-v2). Marked `#[ignore]` to avoid the
    /// ~25 MB ONNX download in default CI; run with:
    /// `cargo test -p crabcc-memory --features memory-embed -- --ignored`.
    #[cfg(feature = "memory-embed")]
    #[test]
    #[ignore = "downloads ~25 MB MiniLM-L6-v2 on first run"]
    fn hybrid_beats_each_ranker_on_distractor_set() {
        use crate::embed::FastEmbedder;
        use crate::SqliteBackend;
        use std::sync::Arc;

        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let backend = Arc::new(SqliteBackend::open(&dir.path().join("memory.db")).unwrap());
        let embedder = Arc::new(FastEmbedder::new().expect("load MiniLM"));
        let p = Palace::with_components(dir.path(), backend, embedder);

        // Corpus: each tuple is (id, body).
        //
        // The semantic-target row deliberately shares ZERO content tokens
        // with its query — only paraphrased meaning. The literal-target
        // row carries a rare identifier `xyzzy_99_42` that BM25 will lock
        // onto but MiniLM has never seen during pre-training.
        let corpus = [
            (
                "sem_target",
                "configuring a connection pool to keep database sessions alive",
            ),
            ("sem_distractor_a", "writing unit tests for parser logic"),
            ("sem_distractor_b", "compile-time macro expansion in Rust"),
            (
                "lit_target",
                "marker token xyzzy_99_42 used to identify a specific drawer",
            ),
            (
                "lit_distractor",
                "naming conventions for unique identifiers in test fixtures",
            ),
            ("noise_a", "the moon orbits the earth roughly every 27 days"),
            (
                "noise_b",
                "espresso extraction temperature is 90-96 celsius",
            ),
        ];
        for (id, body) in corpus {
            p.remember("default", None, id, body)
                .unwrap_or_else(|e| panic!("remember {id}: {e}"));
        }

        // (query, expected, scenario) — the third field labels the
        // semantic / literal axis purely for diagnostic output.
        let queries: &[(&str, &str, &str)] = &[
            (
                "reuse persistent connections so each query does not pay handshake cost",
                "sem_target",
                "semantic",
            ),
            ("xyzzy_99_42", "lit_target", "literal"),
        ];

        for (q, expected, scenario) in queries {
            let lex = p
                .search_with_mode(SearchMode::Lexical, q, 3, None, None)
                .unwrap();
            let vec = p
                .search_with_mode(SearchMode::Vector, q, 3, None, None)
                .unwrap();
            let hyb = p
                .search_with_mode(SearchMode::Hybrid, q, 3, None, None)
                .unwrap();

            let top = |r: &QueryResult| {
                r.hits
                    .first()
                    .map(|h| h.source_id.clone())
                    .unwrap_or_default()
            };
            let (lex_top, vec_top, hyb_top) = (top(&lex), top(&vec), top(&hyb));

            // Hybrid is the contract — it must rank the right drawer at #1
            // for both query shapes.
            assert_eq!(
                hyb_top, *expected,
                "{scenario}: hybrid top != expected ({expected:?}); \
                 lex_top={lex_top:?} vec_top={vec_top:?} hyb_top={hyb_top:?}"
            );

            // The semantic axis is the load-bearing claim: a paraphrased
            // query with zero token overlap MUST miss under BM25, which
            // is precisely why fusion adds value. (Note: on the literal
            // axis, MiniLM's subword tokenization often preserves a
            // unique rare token well enough that vec also wins — we
            // don't assert vec-misses there because that would be a
            // model-specific brittleness, not a fusion contract.)
            if *scenario == "semantic" {
                assert_ne!(
                    lex_top, *expected,
                    "semantic query: lex unexpectedly won — \
                     fixture has too much token overlap; tighten paraphrase"
                );
            }
        }
    }

    // ---- forget (issue #26) -------------------------------------------------

    #[test]
    fn forget_by_id_removes_drawer_and_returns_count() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        let id = p
            .remember("default", None, "doc:1", "to be forgotten")
            .unwrap();
        let other = p
            .remember("default", None, "doc:2", "stays around")
            .unwrap();

        let n = p.forget(&DeleteSel::ById(vec![id])).unwrap();
        assert_eq!(n, 1, "forget should report 1 drawer removed");
        assert!(
            p.get(id).unwrap().is_none(),
            "forgotten drawer must be gone"
        );
        assert!(p.get(other).unwrap().is_some(), "untouched drawer survives");
    }

    #[test]
    fn forget_is_idempotent_on_missing_id() {
        // Issue #26 deliverable — `forget` MUST return Ok with 0 rows
        // removed when the selector matches nothing. Callers (notably
        // the MCP tool) treat this as "drawer is gone, no work needed".
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        let n = p.forget(&DeleteSel::ById(vec![99_999])).unwrap();
        assert_eq!(n, 0, "missing id must return 0, not error");

        // And a second call against a now-empty store stays Ok(0).
        let n = p.forget(&DeleteSel::ById(vec![99_999])).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn forget_before_in_wing_only_drops_matching_rows() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();

        // Seed three drawers across two wings, then backdate two of them
        // via a direct SQLite UPDATE so we can pin `created_at`
        // deterministically without sleeping the test thread.
        let stale = p.remember("notes", None, "old:1", "stale note").unwrap();
        let fresh = p.remember("notes", None, "fresh:1", "fresh note").unwrap();
        let other = p
            .remember("scratch", None, "other:1", "different wing")
            .unwrap();

        let db_path = resolve_db_path(dir.path()).unwrap();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute("UPDATE drawers SET created_at = 500 WHERE id = ?1", [stale])
            .unwrap();
        conn.execute("UPDATE drawers SET created_at = 500 WHERE id = ?1", [other])
            .unwrap();
        conn.execute(
            "UPDATE drawers SET created_at = 2000 WHERE id = ?1",
            [fresh],
        )
        .unwrap();

        let sel = DeleteSel::BeforeInWing {
            wing: "notes".into(),
            before: 1_000,
        };
        let n = p.forget(&sel).unwrap();
        assert_eq!(n, 1, "only `stale` (notes wing, created_at < 1000) drops");

        assert!(p.get(stale).unwrap().is_none(), "stale removed");
        assert!(
            p.get(fresh).unwrap().is_some(),
            "fresh kept (created_at >= cutoff)"
        );
        assert!(p.get(other).unwrap().is_some(), "other-wing row untouched");
    }

    #[test]
    fn forget_before_in_wing_with_no_matches_is_noop() {
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        p.remember("notes", None, "doc:1", "body").unwrap();

        let sel = DeleteSel::BeforeInWing {
            wing: "notes".into(),
            before: 1, // before any plausible created_at
        };
        let n = p.forget(&sel).unwrap();
        assert_eq!(n, 0, "no rows in window must return 0");
        assert_eq!(
            p.count().unwrap(),
            1,
            "drawer survives an empty-window forget"
        );
    }

    // ---- markdown sanitization (issue #54) ---------------------------------

    #[cfg(feature = "markdown")]
    #[test]
    fn remember_sanitizes_drawer_body_with_markdown_feature() {
        // The body arrives with code-fence noise + a heading marker.
        // After remember(), the stored body should be the sanitized
        // form: fence backticks gone, `###` gone, but the identifiers
        // and prose preserved. BM25 / lexical search must still find
        // the content tokens.
        ensure_test_crabcc_home();
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        let raw = "### Connection pool\n\nUse `Store::open` to create a pool:\n\n```rust\nlet s = Store::open(path)?;\n```\n";
        let id = p.remember("default", None, "doc:md", raw).unwrap();

        let drawer = p.get(id).unwrap().expect("drawer present");
        // Markdown syntax tokens stripped.
        assert!(
            !drawer.body.contains("```"),
            "fences leaked: {:?}",
            drawer.body
        );
        assert!(
            !drawer.body.contains("###"),
            "heading marker leaked: {:?}",
            drawer.body
        );
        // Content tokens preserved — these are what BM25 should rank on.
        assert!(drawer.body.contains("Store::open"));
        assert!(drawer.body.contains("Connection pool"));

        // Lexical search still finds the drawer by an identifier that
        // was previously buried inside a code fence.
        let r = p
            .search_with_mode(SearchMode::Lexical, "Store", 5, None, None)
            .unwrap();
        assert!(
            r.hits.iter().any(|h| h.source_id == "doc:md"),
            "lexical search must find the sanitized drawer; hits: {:?}",
            r.hits
        );
    }
}
