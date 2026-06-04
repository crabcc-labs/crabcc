//! Viz-specific runtime helpers — bringing the repo to a "ready" state
//! before serving the dashboard.
//!
//! The agent-PATH helpers (`ensure_bin_dir`, `agent_path`, `CRABCC_BIN_DIR`)
//! moved to `crabcc_core::agent_runtime` so cli can use them without
//! depending on the entire web stack. Re-exported below for any external
//! consumers still calling `crabcc_viz::runtime::*`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

// Back-compat re-exports — these helpers live in `crabcc-core` now.
pub use crabcc_core::agent_runtime::{agent_path, ensure_bin_dir, CRABCC_BIN_DIR};

/// Bootstrap result — what `ensure_initialized` actually did. Useful
/// for the launch banner and for tests that assert side-effect shape
/// without re-implementing `go::init`'s contract here.
#[derive(Debug, Default)]
pub struct InitOutcome {
    pub created_index: bool,
    pub created_graph: bool,
    pub created_memory: bool,
    pub files: usize,
    pub symbols: usize,
    pub graph_edges: usize,
    pub drawers: usize,
}

/// Locate the user's home dir. Mirrors agent.rs / install.rs lookups.
pub fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME not set; cannot resolve ~/.crabcc/"))
}

/// Bring `<root>/.crabcc/{index.db,graph.json,memory.db}` into a
/// consistent "ready" state. Cheap on already-initialized repos
/// (`refresh` does an mtime sweep); does a full index on cold ones.
///
/// Mirrors what `crabcc go` does — same crate (`crabcc-core::index`),
/// same call shape, so future bumps to the init contract apply to
/// both surfaces.
pub fn ensure_initialized(root: &Path) -> Result<InitOutcome> {
    let mut out = InitOutcome::default();

    let crabcc_dir = root.join(".crabcc");
    std::fs::create_dir_all(&crabcc_dir)
        .with_context(|| format!("create {}", crabcc_dir.display()))?;

    let db = crabcc_dir.join("index.db");
    out.created_index = !db.exists();
    let store = crabcc_core::store::Store::open(&db).context("open .crabcc/index.db")?;
    if out.created_index {
        let stats = crabcc_core::index::full_index(root, &store)?;
        out.files = stats.files_indexed;
        out.symbols = stats.symbols;
    } else {
        let _ = crabcc_core::index::refresh(root, &store)?;
        out.files = store.list_files().map(|v| v.len()).unwrap_or_default();
        out.symbols = store
            .iter_all_symbols()
            .map(|v| v.len())
            .unwrap_or_default();
    }

    // Graph sidecar — rebuild on cold init, leave the cached version
    // alone otherwise (refresh doesn't invalidate edges; rebuilding
    // each time would be wasted work on warm runs).
    let graph_path = crabcc_dir.join("graph.json");
    if !graph_path.exists() {
        out.created_graph = true;
        let g = crabcc_core::graph::CallGraph::build(&store, root)?;
        g.save(&graph_path)?;
        out.graph_edges = g.edge_count;
    } else if let Ok(g) = crabcc_core::graph::CallGraph::load(&graph_path) {
        out.graph_edges = g.edge_count;
    }

    // Memory db — touch via Palace::open. Idempotent; bootstraps the
    // schema if the file is missing.
    let memory_path = crabcc_dir.join("memory.db");
    out.created_memory = !memory_path.exists();
    if let Ok(palace) = crabcc_memory::palace::Palace::open(root) {
        out.drawers = palace.count().unwrap_or_default();
    }

    // Service-discovery sidecar (issue #143). Best-effort — readonly fs
    // in a container shouldn't break serve init.
    let report = crabcc_core::service_discovery::discover_all();
    let _ = crabcc_core::service_discovery::write_sidecar(root, &report);

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_initialized_creates_index_and_graph() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "pub fn outer() { inner(); }\npub fn inner() {}\n",
        )
        .unwrap();
        let outcome = ensure_initialized(dir.path()).unwrap();
        assert!(outcome.created_index);
        assert!(outcome.created_graph);
        assert!(dir.path().join(".crabcc/index.db").exists());
        assert!(dir.path().join(".crabcc/graph.json").exists());
        // Issue #143 — services.json sidecar round-trips through the
        // service_discovery public API.
        let report =
            crabcc_core::service_discovery::read_sidecar(dir.path()).expect("services.json");
        assert!(!report.services.is_empty());
    }
}
