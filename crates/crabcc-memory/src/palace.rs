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

use crate::backend::{sqlite::SqliteBackend, Backend};
use crate::embed::{Embedder, HashEmbedder};
use crate::types::*;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

    /// Embed query text and search top-K (no filters).
    pub fn search(&self, query: &str, limit: usize) -> Result<QueryResult> {
        self.search_filtered(query, limit, None, None)
    }

    /// Embed query text and search top-K with optional wing/room filters.
    pub fn search_filtered(
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
pub struct PalaceRegistry {
    palaces: Mutex<HashMap<PathBuf, Arc<Palace>>>,
}

impl PalaceRegistry {
    pub fn new() -> Self {
        Self {
            palaces: Mutex::new(HashMap::new()),
        }
    }

    /// Look up the palace for `cwd_or_root`. If `cwd_or_root` isn't itself
    /// a git root, walks up to find one. Falls back to using the path as-is
    /// if no `.git` is found upstream.
    pub fn open_for(&self, cwd_or_root: &Path) -> Result<Arc<Palace>> {
        let root = find_git_root(cwd_or_root).unwrap_or_else(|| cwd_or_root.to_path_buf());
        let canon = root.canonicalize().unwrap_or(root.clone());
        let mut map = self
            .palaces
            .lock()
            .map_err(|_| anyhow::anyhow!("poisoned"))?;
        if let Some(p) = map.get(&canon) {
            return Ok(p.clone());
        }
        let p = Arc::new(Palace::open(&canon)?);
        map.insert(canon.clone(), p.clone());
        Ok(p)
    }

    pub fn count(&self) -> usize {
        self.palaces.lock().map(|m| m.len()).unwrap_or(0)
    }
}

impl Default for PalaceRegistry {
    fn default() -> Self {
        Self::new()
    }
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
}
