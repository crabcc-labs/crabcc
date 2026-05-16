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
use std::collections::{HashMap, HashSet};
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
    /// v4: takes a `symbol_id` (i64), resolves to symbol name via
    /// `symbol_name_by_id`, then looks up metadata via `find_symbol`.
    pub(crate) fn from_id_with_store(symbol_id: i64, depth: usize, store: &Store) -> Self {
        let name = store
            .symbol_name_by_id(symbol_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| symbol_id.to_string());
        let metadata = crabcc_core::query::find_symbol(store, &name)
            .ok()
            .and_then(|hits| hits.into_iter().next());
        match metadata {
            Some(sym) => Self {
                id: name,
                depth,
                kind: Some(symbol_kind_str(&sym.kind).to_string()),
                file: Some(sym.file),
                line: Some(sym.line_start),
                signature: sym.signature,
            },
            None => Self {
                id: name,
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

    let db = root.join(".crabcc").join("index.db");
    let store = Store::open(&db).with_context(|| format!("opening store at {}", db.display()))?;
    let graph_path = root.join(".crabcc").join("graph.json");
    let graph = if graph_path.exists() {
        CallGraph::load(&graph_path)?
    } else {
        CallGraph::build(&store, root)?
    };

    // v4: resolve root name to symbol_id before the BFS walk.
    let root_id = store
        .symbol_id_by_name(&q.root)?
        .with_context(|| format!("symbol '{}' not found in index", q.root))?;

    let dir = q.dir.as_str();
    let frontier: Vec<GraphHit> = match dir {
        "callees" => graph.outgoing(root_id, depth),
        _ => graph.incoming(root_id, depth),
    };

    let mut nodes: Vec<NodeOut> =
        std::iter::once(NodeOut::from_id_with_store(root_id, 0, &store))
            .chain(
                frontier
                    .into_iter()
                    .map(|h| NodeOut::from_id_with_store(h.symbol_id, h.depth, &store)),
            )
            .collect();
    let truncated = nodes.len() > MAX_NODES;
    if truncated {
        nodes.truncate(MAX_NODES);
    }

    // v4: graph adjacency is keyed by i64. Resolve each node's name back
    // to a symbol_id for edge building, and build an in-set of ids for
    // the induced-subgraph filter.
    let mut name_to_id: HashMap<String, i64> = HashMap::new();
    for n in &nodes {
        if let Ok(Some(sid)) = store.symbol_id_by_name(&n.id) {
            name_to_id.insert(n.id.clone(), sid);
        }
    }
    let in_set: HashSet<i64> = name_to_id.values().copied().collect();

    let mut edges: Vec<EdgeOut> = Vec::with_capacity(nodes.len() * 2);
    for n in &nodes {
        if let Some(sid) = name_to_id.get(&n.id) {
            if dir == "callees" {
                if let Some(neighbors) = graph.callees.get(sid) {
                    for nb in neighbors {
                        if in_set.contains(nb) {
                            let nb_name = store
                                .symbol_name_by_id(*nb)
                                .ok()
                                .flatten()
                                .unwrap_or_else(|| nb.to_string());
                            edges.push(EdgeOut {
                                src: n.id.clone(),
                                dst: nb_name,
                            });
                        }
                    }
                }
            } else if let Some(neighbors) = graph.callers.get(sid) {
                for nb in neighbors {
                    if in_set.contains(nb) {
                        let nb_name = store
                            .symbol_name_by_id(*nb)
                            .ok()
                            .flatten()
                            .unwrap_or_else(|| nb.to_string());
                        edges.push(EdgeOut {
                            src: nb_name,
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
