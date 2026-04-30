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
use moka::sync::Cache;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    /// Open or create a persistent palace at `<repo_root>/.crabcc/memory.db`.
    /// Default embedder is `HashEmbedder` until M1 wires `fastembed-rs`.
    pub fn open(repo_root: &Path) -> Result<Self> {
        let crabcc_dir = repo_root.join(".crabcc");
        std::fs::create_dir_all(&crabcc_dir)
            .with_context(|| format!("create {}", crabcc_dir.display()))?;
        let db_path = crabcc_dir.join("memory.db");
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
    pub fn remember_in_session(
        &self,
        wing: &str,
        room: Option<&str>,
        source_id: &str,
        body: &str,
        session_id: Option<&str>,
    ) -> Result<DrawerId> {
        let emb = self.embedder.embed_one(body)?;
        let ids = self.backend.add(&[DrawerInsert {
            wing: wing.into(),
            room: room.map(str::to_string),
            source_id: source_id.into(),
            body: body.into(),
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

    /// Drawer count.
    pub fn count(&self) -> Result<usize> {
        self.backend.count()
    }

    /// Health snapshot.
    pub fn health(&self) -> HealthStatus {
        self.backend.health()
    }
}

/// Walk up from `start` looking for `.git/`. Returns the first ancestor
/// containing one, or `None` if not in a git repo.
///
/// Hot paths (the MCP tool dispatch loop) should prefer routing through
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
        match p.parent() {
            Some(parent) => p = parent.to_path_buf(),
            None => return None,
        }
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

    #[test]
    fn open_creates_crabcc_dir_and_db() {
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        assert!(p.root.join(".crabcc").exists());
        assert!(p.root.join(".crabcc").join("memory.db").exists());
    }

    #[test]
    fn open_is_idempotent_and_reuses_data() {
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
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let nested = dir.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        let found = find_git_root(&nested).unwrap();
        assert_eq!(found, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn registry_caches_per_root() {
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
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        assert!(p.search("anything", 5).unwrap().hits.is_empty());
    }

    #[test]
    fn registry_concurrent_open_for_same_root() {
        // 4 threads race to open_for the same git root. Registry must
        // hand out the same Arc to all of them and end with count == 1.
        use std::thread;
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
        let dir = tempdir().unwrap();
        let p = Palace::open(dir.path()).unwrap();
        let id = p.remember("default", None, "doc:1", "hello").unwrap();
        let d = p.get(id).unwrap().expect("drawer present");
        assert!(d.session_id.is_none());
    }

    #[test]
    fn remember_in_session_persists_across_reopen() {
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
            ("http:1", "configure listen address and port for the HTTP server"),
            ("http:2", "rate limiting middleware for inbound HTTP requests"),
            ("http:3", "TLS termination and certificate auto-renewal via ACME"),
            ("http:4", "CORS headers for cross-origin browser requests"),
            ("http:5", "websocket upgrade handshake on the HTTP listener"),
            // cluster 2: SQL / databases
            ("db:1", "create a new postgres database with utf8 encoding"),
            ("db:2", "alter table to add a non-null default column"),
            ("db:3", "transaction isolation levels for sqlite WAL mode"),
            ("db:4", "rollback strategy for a failed batch insert into postgres"),
            ("db:5", "connection pooling for high-throughput SQL workloads"),
            // cluster 3: tree-sitter / parsing
            ("ts:1", "tree-sitter walks symbol tables in source code"),
            ("ts:2", "ast-grep patterns for matching function call sites"),
            ("ts:3", "incremental reparse on edit using tree-sitter"),
            ("ts:4", "node kind dispatch table for typescript and javascript"),
            ("ts:5", "tree-sitter-rust grammar handles impl blocks"),
            // cluster 4: embeddings / vectors
            ("vec:1", "MiniLM-L6-v2 produces 384-dim semantic embeddings"),
            ("vec:2", "cosine similarity over L2 normalized float vectors"),
            ("vec:3", "sqlite-vec virtual table for ANN nearest neighbor search"),
            ("vec:4", "fastembed batch embed reuses the ort session"),
            ("vec:5", "FNV xorshift fallback for deterministic test embeddings"),
            // cluster 5: filesystem / IO
            ("fs:1", "ignore-aware repository walk respecting gitignore"),
            ("fs:2", "memchr fast prefilter for byte needle in source files"),
            ("fs:3", "mmap large index database read only access"),
            ("fs:4", "filesystem watcher debouncing rapid file save bursts"),
            ("fs:5", "atomic write to temp file then rename for durability"),
            // cluster 6: compression
            ("zip:1", "FSST symbol table compresses repeated string prefixes"),
            ("zip:2", "zstd level tradeoff between size and decompression speed"),
            ("zip:3", "decode FSST encoded rows lazily on read"),
            ("zip:4", "bench compression ratio across natural language and code"),
            ("zip:5", "cargo feature gate for optional fsst-rs dependency"),
            // cluster 7: caching
            ("cache:1", "moka time to idle eviction closes idle SQLite connections"),
            ("cache:2", "sha256 keyed cache for repeated embedding queries"),
            ("cache:3", "git root memoization with a 60 second TTL"),
            ("cache:4", "bounded LRU palace registry keyed by canonical repo root"),
            ("cache:5", "FSST codec arc shared across store sessions no contention"),
            // cluster 8: testing
            ("test:1", "tempfile based integration test cleans up in drop"),
            ("test:2", "ignore attribute for network dependent end to end checks"),
            ("test:3", "snapshot comparison for golden file fixtures"),
            ("test:4", "criterion benchmark harness reports throughput"),
            ("test:5", "table driven tests with parameterized search queries"),
            // cluster 9: tokens / shaping
            ("tok:1", "tokens per query saved versus raw grep walk"),
            ("tok:2", "limit flag short circuits per file walk early stop"),
            ("tok:3", "files only output dedupes hits across the same file"),
            ("tok:4", "count mode emits only an integer not the matched lines"),
            ("tok:5", "JSON projection via jq pipeline trims irrelevant fields"),
            // cluster 10: misc / off-topic
            ("misc:1", "release pipeline builds linux mac windows binaries"),
            ("misc:2", "homebrew tap distributes prebuilt mac arm64 binaries"),
            ("misc:3", "starship custom module renders crabcc status in the prompt"),
            ("misc:4", "claude code skill auto routes lookups to crabcc CLI"),
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
                eprintln!(
                    "miss: query={query:?} expected={expected:?} top5={ids:?}"
                );
            }
        }

        let recall = hits as f64 / queries.len() as f64;
        assert!(
            recall >= 0.8,
            "recall@5 = {recall:.2} (got {hits}/{}); issue #20 requires ≥ 0.80",
            queries.len()
        );
    }
}
