//! `/api/bootstrap` — one-shot "what does the live dashboard need to
//! know on first paint?" snapshot.
//!
//! Combines repo metadata with index sidecar stats so the header
//! section can render before we wait on `/api/activity`. Fast: a cold
//! call against an indexed repo measures sub-50ms.

use anyhow::Result;
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
pub(crate) struct BootstrapSnapshot {
    repo: String,
    root: String,
    version: &'static str,
    index: IndexState,
    graph: GraphState,
    memory: MemoryState,
}

#[derive(Serialize)]
struct IndexState {
    present: bool,
    files: usize,
    symbols: usize,
    edges: usize,
    db_bytes: u64,
    db_mtime: u64,
}

#[derive(Serialize)]
struct GraphState {
    present: bool,
    edges: usize,
    callers: usize,
    callees: usize,
}

#[derive(Serialize)]
struct MemoryState {
    present: bool,
    drawers: usize,
}

pub(crate) fn bootstrap_snapshot(root: &Path) -> Result<BootstrapSnapshot> {
    let repo = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();
    let db_path = root.join(".crabcc").join("index.db");
    let graph_path = root.join(".crabcc").join("graph.json");
    // Memory db moved to $CRABCC_HOME/repos/<slug>-<hash6>/memory.db
    // (#479) so worktrees of one repo share a drawer store and
    // `git clean -fdx` doesn't blow it away. resolve_db_path is the
    // single source of truth for the layout.
    let memory_path = crabcc_memory::resolve_db_path(root)
        .unwrap_or_else(|_| root.join(".crabcc").join("memory.db"));

    let mut index = IndexState {
        present: db_path.exists(),
        files: 0,
        symbols: 0,
        edges: 0,
        db_bytes: 0,
        db_mtime: 0,
    };
    if let Ok(meta) = std::fs::metadata(&db_path) {
        index.db_bytes = meta.len();
        index.db_mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or_default();
    }
    if index.present {
        // Open in read-only-ish fashion via Store — costs about a stat
        // plus three count(*) round-trips, all cheap on an indexed db.
        if let Ok(store) = crabcc_core::store::Store::open(&db_path) {
            index.files = store.list_files().map(|v| v.len()).unwrap_or_default();
            index.symbols = store
                .iter_all_symbols()
                .map(|v| v.len())
                .unwrap_or_default();
            index.edges = store.edge_count().map(|n| n as usize).unwrap_or_default();
        }
    }

    let mut graph = GraphState {
        present: graph_path.exists(),
        edges: 0,
        callers: 0,
        callees: 0,
    };
    if graph.present {
        if let Ok(g) = crabcc_core::graph::CallGraph::load(&graph_path) {
            graph.edges = g.edge_count;
            graph.callers = g.callers.len();
            graph.callees = g.callees.len();
        }
    }

    let mut memory = MemoryState {
        present: memory_path.exists(),
        drawers: 0,
    };
    if memory.present {
        // Palace::open does its own bootstrap; we don't want a fresh
        // schema-create as a side effect of a viewer GET. Drop into the
        // raw rusqlite path used by the backend instead.
        if let Ok(conn) = rusqlite::Connection::open_with_flags(
            &memory_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            if let Ok(n) =
                conn.query_row("select count(*) from drawers", [], |r| r.get::<_, i64>(0))
            {
                memory.drawers = n as usize;
            }
        }
    }

    Ok(BootstrapSnapshot {
        repo,
        root: root.display().to_string(),
        version: env!("CARGO_PKG_VERSION"),
        index,
        graph,
        memory,
    })
}
