//! Knowledge-base graph sidecar — caller/callee relationships over
//! symbol-IDs (v4).
//!
//! - v4 keys adjacency by `SymbolId`, not by symbol name. This
//!   restores resolution: `Foo::open` and `Bar::open` are distinct nodes,
//!   so transitive walks don't collapse them.
//! - Built from the `edges` table populated at extract time by the
//!   two-pass extractor + per-language resolvers (Tasks 4–7). One
//!   `iter_call_edges_resolved` scan folds `(src_symbol_id,
//!   dst_symbol_id)` pairs straight into adjacency. The v1.0.0 legacy
//!   build path (`build_legacy`) is removed — v4 indexes always populate
//!   the `edges` table on first open.
//! - Persisted to `.crabcc/graph.json`. JSON nodes are symbol-IDs;
//!   render-time name resolution is the responsibility of the caller
//!   (the CLI dispatcher in `main.rs` joins against `symbols` to print
//!   human-readable qualified names alongside IDs).
//! - BFS expansion is depth-bounded; hash-set dedup avoids cycles.
//! - `cycles()` returns non-trivial SCCs (Tarjan, iterative);
//!   `orphans()` returns symbol-IDs with outgoing edges but no incoming.

use crate::resolve::SymbolId;
use crate::store::{CallEdge, Store};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::Path;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CallGraph {
    /// Outgoing: caller symbol_id -> set of callee symbol_ids.
    pub callees: BTreeMap<SymbolId, BTreeSet<SymbolId>>,
    /// Reverse: callee symbol_id -> set of caller symbol_ids (computed on load).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub callers: BTreeMap<SymbolId, BTreeSet<SymbolId>>,
    /// Total number of edges. For sanity checks + reporting.
    #[serde(default)]
    pub edge_count: usize,
}

#[derive(Debug, Serialize)]
pub struct GraphHit {
    pub symbol_id: SymbolId,
    pub depth: usize,
}

impl CallGraph {
    /// Build the call graph from the v4 `edges` table. One SQL scan of
    /// `(src_symbol_id, dst_symbol_id)` pairs; no per-file walks, no
    /// parser invocations.
    pub fn build(store: &Store, _root: &Path) -> Result<Self> {
        let t0 = std::time::Instant::now();
        let mut g = Self::default();
        for CallEdge { src, dst, .. } in store.iter_call_edges_resolved()? {
            let inserted = g.callees.entry(src).or_default().insert(dst);
            g.callers.entry(dst).or_default().insert(src);
            if inserted {
                g.edge_count += 1;
            }
        }
        let nodes = g.callees.len() + g.callers.len();
        tracing::info!(
            target: "crabcc_core::graph",
            kpi = "graph.build",
            edges = g.edge_count,
            nodes,
            duration_ms = t0.elapsed().as_millis() as u64,
            "graph build done"
        );
        Ok(g)
    }

    /// BFS over outgoing edges starting at `start_id`.
    pub fn outgoing(&self, start_id: SymbolId, depth: usize) -> Vec<GraphHit> {
        let t0 = std::time::Instant::now();
        let r = bfs(&self.callees, start_id, depth);
        tracing::info!(
            target: "crabcc_core::graph",
            kpi = "graph.walk",
            direction = "outgoing",
            depth,
            frontier = r.len(),
            duration_ms = t0.elapsed().as_millis() as u64,
            "graph walk done"
        );
        r
    }

    /// BFS over reverse edges (who calls `start_id`?).
    pub fn incoming(&self, start_id: SymbolId, depth: usize) -> Vec<GraphHit> {
        let t0 = std::time::Instant::now();
        let r = bfs(&self.callers, start_id, depth);
        tracing::info!(
            target: "crabcc_core::graph",
            kpi = "graph.walk",
            direction = "incoming",
            depth,
            frontier = r.len(),
            duration_ms = t0.elapsed().as_millis() as u64,
            "graph walk done"
        );
        r
    }

    /// Strongly connected components of size >= 2 (mutual recursion / cycles).
    /// Returned components are sorted by ascending symbol_id; the outer list
    /// is sorted by first-element id for stable output.
    pub fn cycles(&self) -> Vec<Vec<SymbolId>> {
        let t0 = std::time::Instant::now();
        let mut sccs = tarjan_scc(&self.callees);
        sccs.retain(|c| c.len() >= 2);
        for c in &mut sccs {
            c.sort();
        }
        sccs.sort_by(|a, b| a[0].cmp(&b[0]));
        tracing::info!(
            target: "crabcc_core::graph",
            kpi = "graph.cycles",
            count = sccs.len(),
            duration_ms = t0.elapsed().as_millis() as u64,
            "graph cycles done"
        );
        sccs
    }

    /// IDs that are in `callees` (i.e. they call something) but never appear
    /// as a callee. Dead-code triage starting point.
    pub fn orphans(&self) -> Vec<SymbolId> {
        let t0 = std::time::Instant::now();
        let mut out: Vec<SymbolId> = self
            .callees
            .keys()
            .copied()
            .filter(|k| !self.callers.contains_key(k))
            .collect();
        out.sort();
        tracing::info!(
            target: "crabcc_core::graph",
            kpi = "graph.orphans",
            count = out.len(),
            duration_ms = t0.elapsed().as_millis() as u64,
            "graph orphans done"
        );
        out
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).ok();
        }
        let body = serde_json::to_vec_pretty(self).context("serialize graph")?;
        std::fs::write(path, body).context("write graph.json")?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let body = std::fs::read(path).context("read graph.json")?;
        let mut g: Self = serde_json::from_slice(&body).context("parse graph.json")?;
        // Reverse map may have been omitted on disk — rebuild if so.
        if g.callers.is_empty() {
            for (caller, callees) in &g.callees {
                for callee in callees {
                    g.callers.entry(*callee).or_default().insert(*caller);
                }
            }
        }
        Ok(g)
    }
}

fn bfs(
    adj: &BTreeMap<SymbolId, BTreeSet<SymbolId>>,
    start: SymbolId,
    depth: usize,
) -> Vec<GraphHit> {
    let mut out = Vec::new();
    let mut seen: HashSet<SymbolId> = HashSet::new();
    let mut q: VecDeque<(SymbolId, usize)> = VecDeque::new();
    q.push_back((start, 0));
    seen.insert(start);
    while let Some((node, d)) = q.pop_front() {
        if d > 0 {
            out.push(GraphHit {
                symbol_id: node,
                depth: d,
            });
        }
        if d >= depth {
            continue;
        }
        if let Some(neighbours) = adj.get(&node) {
            for n in neighbours {
                if seen.insert(*n) {
                    q.push_back((*n, d + 1));
                }
            }
        }
    }
    out
}

/// Tarjan's strongly-connected-components — iterative implementation to
/// avoid blowing the stack on deep call chains.
fn tarjan_scc(adj: &BTreeMap<SymbolId, BTreeSet<SymbolId>>) -> Vec<Vec<SymbolId>> {
    let mut idx: BTreeMap<SymbolId, usize> = BTreeMap::new();
    let mut lowlink: BTreeMap<SymbolId, usize> = BTreeMap::new();
    let mut on_stack: HashSet<SymbolId> = HashSet::new();
    let mut stack: Vec<SymbolId> = Vec::new();
    let mut next_index: usize = 0;
    let mut sccs: Vec<Vec<SymbolId>> = Vec::new();

    enum Frame {
        Enter(SymbolId),
        Resume {
            node: SymbolId,
            iter_pos: usize,
            children: Vec<SymbolId>,
        },
    }

    let mut nodes: Vec<SymbolId> = adj.keys().copied().collect();
    nodes.sort();
    for start in nodes {
        if idx.contains_key(&start) {
            continue;
        }
        let mut work: Vec<Frame> = vec![Frame::Enter(start)];
        while let Some(frame) = work.pop() {
            match frame {
                Frame::Enter(node) => {
                    idx.insert(node, next_index);
                    lowlink.insert(node, next_index);
                    next_index += 1;
                    stack.push(node);
                    on_stack.insert(node);
                    let children: Vec<SymbolId> = adj
                        .get(&node)
                        .map(|s| s.iter().copied().collect())
                        .unwrap_or_default();
                    work.push(Frame::Resume {
                        node,
                        iter_pos: 0,
                        children,
                    });
                }
                Frame::Resume {
                    node,
                    mut iter_pos,
                    children,
                } => {
                    let mut descended = false;
                    while iter_pos < children.len() {
                        let child = children[iter_pos];
                        iter_pos += 1;
                        if !idx.contains_key(&child) {
                            work.push(Frame::Resume {
                                node,
                                iter_pos,
                                children: children.clone(),
                            });
                            work.push(Frame::Enter(child));
                            descended = true;
                            break;
                        } else if on_stack.contains(&child) {
                            let cl = lowlink[&child];
                            let nl = lowlink[&node];
                            lowlink.insert(node, nl.min(cl));
                        }
                    }
                    if descended {
                        continue;
                    }
                    if lowlink[&node] == idx[&node] {
                        let mut comp = Vec::new();
                        loop {
                            let w = stack.pop().expect("stack underflow in tarjan");
                            on_stack.remove(&w);
                            let is_root = w == node;
                            comp.push(w);
                            if is_root {
                                break;
                            }
                        }
                        sccs.push(comp);
                    }
                    if let Some(Frame::Resume { node: parent, .. }) = work.last() {
                        let cl = lowlink[&node];
                        let pl = lowlink[parent];
                        let parent = *parent;
                        lowlink.insert(parent, pl.min(cl));
                    }
                }
            }
        }
    }
    sccs
}
