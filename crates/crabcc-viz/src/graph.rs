//! `/api/graph` snapshot — bounded BFS over the cached call graph.
//!
//! Returns the induced subgraph (nodes + their inter-edges) needed by
//! the interactive canvas, with each node enriched from the symbol
//! store so the desktop drawer can render the full header without a
//! follow-up RPC.

use crate::query::parse_query;
use crate::{MAX_DEPTH, MAX_NODES};
use anyhow::{Context, Result};
use crabcc_core::graph::{CallGraph, GraphHit};
use crabcc_core::store::Store;
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
pub(crate) struct GraphSnapshot {
    root: String,
    dir: String,
    depth: usize,
    truncated: bool,
    nodes: Vec<NodeOut>,
    edges: Vec<EdgeOut>,
}

#[derive(Serialize)]
pub(crate) struct NodeOut {
    id: String,
    depth: usize,
    /// Symbol kind when the node id resolves to an indexed symbol —
    /// `function` / `struct` / `enum` / `trait` / `const` / `type` /
    /// `macro`. `None` for nodes whose id couldn't be matched (call
    /// targets the indexer didn't catch — extern crate fns, std, etc).
    /// Added in #301 so the desktop graph drawer can render a kind
    /// badge without a follow-up RPC.
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    /// Repo-relative file path of the symbol's defining site.
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    /// 1-based line number of the symbol's defining site.
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<u32>,
    /// Single-line signature (e.g. `pub fn open(path: &Path) -> Result<Store>`).
    /// `None` when the indexer didn't capture one (rare for fns,
    /// common for type aliases / consts depending on language plugin).
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
}

impl NodeOut {
    /// Build a `NodeOut` and try to enrich it from the symbol index.
    /// Looks up `id` via [`crabcc_core::query::find_symbol`] and takes
    /// the first match (callers / call targets are referenced by name,
    /// so multiple definitions with the same name simply pick one
    /// deterministic choice — same trade-off `crabcc sym` makes).
    pub(crate) fn from_id_with_store(id: String, depth: usize, store: &Store) -> Self {
        let metadata = crabcc_core::query::find_symbol(store, &id)
            .ok()
            .and_then(|hits| hits.into_iter().next());
        match metadata {
            Some(sym) => Self {
                id,
                depth,
                kind: Some(symbol_kind_str(&sym.kind).to_string()),
                file: Some(sym.file),
                line: Some(sym.line_start),
                signature: sym.signature,
            },
            None => Self {
                id,
                depth,
                kind: None,
                file: None,
                line: None,
                signature: None,
            },
        }
    }
}

/// Map [`crabcc_core::types::SymbolKind`] to the wire string used in
/// the seed-graph response. Mirrors the enum's `#[serde(rename_all =
/// "snake_case")]` Serialize impl exactly so the wire shape is
/// identical to whatever `crabcc_core` emits elsewhere. Keep in
/// lockstep with the openapi spec's `GraphNode.kind` enum.
pub(crate) fn symbol_kind_str(k: &crabcc_core::types::SymbolKind) -> &'static str {
    use crabcc_core::types::SymbolKind as K;
    match k {
        K::Function => "function",
        K::Method => "method",
        K::Class => "class",
        K::Struct => "struct",
        K::Enum => "enum",
        K::Trait => "trait",
        K::Interface => "interface",
        K::Const => "const",
        K::Var => "var",
        K::Type => "type",
        K::Macro => "macro",
    }
}

#[derive(Serialize)]
pub(crate) struct EdgeOut {
    pub src: String,
    pub dst: String,
}

/// Build a bounded BFS snapshot of the call graph for the given root symbol.
///
/// The raw `CallGraph::incoming` / `CallGraph::outgoing` return only the
/// frontier symbol names + their depths; the viewer additionally needs the
/// edges *between* those nodes so the canvas layout has something to render.
/// We materialize the induced subgraph here by walking each node's outgoing
/// (or incoming) adjacency and keeping only edges where both endpoints are
/// in the BFS frontier.
pub(crate) fn graph_snapshot(root: &Path, query: &str) -> Result<GraphSnapshot> {
    let q = parse_query(query)?;
    let depth = q.depth.min(MAX_DEPTH);

    // Open the SQLite store and the cached graph. We don't try to refresh
    // the index here — `crabcc serve` is a viewer, not an indexer; users
    // run `crabcc index` / `crabcc refresh` separately. (Phase 2 will push
    // a "stale index" notice over WebSocket when the on-disk db mtime moves.)
    let db = root.join(".crabcc").join("index.db");
    let store = Store::open(&db).with_context(|| format!("opening store at {}", db.display()))?;
    let graph_path = root.join(".crabcc").join("graph.json");
    let graph = if graph_path.exists() {
        CallGraph::load(&graph_path)?
    } else {
        CallGraph::build(&store, root)?
    };

    let dir = q.dir.as_str();

    // Resolve root name → symbol ID for graph traversal. CallGraph v4 uses
    // i64 IDs; names are bridge-resolved via the Store at call and return time.
    let root_id = store
        .symbol_id_by_name(&q.root)?
        .ok_or_else(|| anyhow::anyhow!("symbol {:?} not found in call graph", q.root))?;
    let mut id_to_name: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    id_to_name.insert(root_id, q.root.clone());
    let frontier: Vec<GraphHit> = match dir {
        "callees" => graph.outgoing(root_id, depth),
        _ => graph.incoming(root_id, depth),
    };

    // Resolve each BFS hit's symbol_id back to a name.
    let frontier_names: Vec<(String, usize)> = frontier
        .into_iter()
        .filter_map(|h| {
            let name = store.symbol_name_by_id(h.symbol_id).ok()??;
            id_to_name.insert(h.symbol_id, name.clone());
            Some((name, h.depth))
        })
        .collect();

    // The frontier from `incoming` / `outgoing` excludes the root itself.
    // Add it back at depth 0 so the canvas has a recognizable focus point.
    // Each node is enriched with kind / file / line / signature via
    // `NodeOut::from_id_with_store` (#301) so the desktop drawer can
    // render the full symbol header without a follow-up RPC.
    let mut nodes: Vec<NodeOut> =
        std::iter::once(NodeOut::from_id_with_store(q.root.clone(), 0, &store))
            .chain(
                frontier_names
                    .into_iter()
                    .map(|(name, depth)| NodeOut::from_id_with_store(name, depth, &store)),
            )
            .collect();
    let truncated = nodes.len() > MAX_NODES;
    if truncated {
        nodes.truncate(MAX_NODES);
    }

    // Build name→id reverse map (only for nodes in the final set) for edge
    // materialization.
    let name_to_id: std::collections::HashMap<&str, i64> = id_to_name
        .iter()
        .map(|(id, name)| (name.as_str(), *id))
        .collect();
    let in_set: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let mut edges: Vec<EdgeOut> = Vec::with_capacity(nodes.len() * 2);
    for n in &nodes {
        let Some(&sym_id) = name_to_id.get(n.id.as_str()) else {
            continue;
        };
        // For a `callees` view we draw edges in the call direction
        // (root → callee), and for `callers` we draw caller → root. The
        // direction of the arrow visualizes "who calls whom" in both modes.
        if dir == "callees" {
            if let Some(neighbors) = graph.callees.get(&sym_id) {
                for &nb_id in neighbors {
                    if let Some(nb_name) = id_to_name.get(&nb_id) {
                        if in_set.contains(nb_name.as_str()) {
                            edges.push(EdgeOut {
                                src: n.id.clone(),
                                dst: nb_name.clone(),
                            });
                        }
                    }
                }
            }
        } else if let Some(neighbors) = graph.callers.get(&sym_id) {
            for &nb_id in neighbors {
                if let Some(nb_name) = id_to_name.get(&nb_id) {
                    if in_set.contains(nb_name.as_str()) {
                        edges.push(EdgeOut {
                            src: nb_name.clone(),
                            dst: n.id.clone(),
                        });
                    }
                }
            }
        }
    }

    Ok(GraphSnapshot {
        root: q.root,
        dir: q.dir,
        depth,
        truncated,
        nodes,
        edges,
    })
}
