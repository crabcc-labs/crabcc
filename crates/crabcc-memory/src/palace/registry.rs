//! Multi-project palace cache. The MCP server keeps one [`PalaceRegistry`]
//! per process and routes each incoming tool call's `cwd` arg through it.

use super::path::find_git_root;
use super::Palace;
use anyhow::Result;
use moka::sync::Cache;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

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
const GIT_ROOT_CACHE_CAPACITY: u64 = 256;

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
