//! Knowledge-base graph sidecar — caller/callee relationships.
//!
//! - The graph is an adjacency map `caller -> {callee, …}` keyed by symbol
//!   name. Names are unqualified — we don't resolve receiver/module yet, so
//!   `Foo.bar()` and `Bar.bar()` collapse to `bar`. Agents asking "what
//!   calls bar?" usually want both.
//! - **v2.0**: built from the `edges` table populated at extract time. One
//!   `iter_call_edges` scan replaces the v1.0.0 O(symbols × files) loop that
//!   ran `query::find_callers` per symbol. Pre-v2 indexes (edges empty)
//!   transparently fall back to the legacy walker.
//! - Persisted to `.crabcc/graph.json`. `crabcc graph build` rebuilds;
//!   `crabcc graph walk NAME` loads + queries. Falls back to live BFS if no
//!   cache exists.
//! - BFS expansion is depth-bounded; hash-set dedup avoids cycles.
//! - `cycles()` returns non-trivial SCCs (Tarjan); `orphans()` returns
//!   defined symbols with no incoming edges (dead-code candidates).

use crate::query;
use crate::store::Store;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::Path;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CallGraph {
    /// Outgoing: caller symbol name -> set of callee symbol names.
    pub callees: BTreeMap<String, BTreeSet<String>>,
    /// Reverse: callee name -> set of callers (computed on load).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub callers: BTreeMap<String, BTreeSet<String>>,
    /// Total number of edges. For sanity checks + reporting.
    #[serde(default)]
    pub edge_count: usize,
}

#[derive(Debug, Serialize)]
pub struct GraphHit {
    pub name: String,
    pub depth: usize,
}

impl CallGraph {
    /// Build the call graph. v2.0+: a single scan of the populated `edges`
    /// table folds (src_symbol, dst_name) pairs straight into adjacency.
    /// Legacy path (v1.0.0 indexes with empty edges) walks symbols × files
    /// and ast-grep — kept as a transparent fallback so upgrading users
    /// don't have to re-index before `graph-build` works.
    pub fn build(store: &Store, root: &Path) -> Result<Self> {
        if store.meta_get("edges_populated")?.as_deref() == Some("1") {
            return Self::build_from_edges(store);
        }
        Self::build_legacy(store, root)
    }

    /// Pure-SQL build path. Each `(caller, callee)` row already represents one
    /// call edge — no per-file walks, no parser invocations. The cost is one
    /// `SELECT … FROM edges` plus the BTreeMap inserts.
    pub fn build_from_edges(store: &Store) -> Result<Self> {
        let mut g = Self::default();
        for (caller, callee) in store.iter_call_edges()? {
            // De-dupe via BTreeSet insert: edge_count counts unique pairs,
            // matching the legacy build's behaviour.
            let inserted = g
                .callees
                .entry(caller.clone())
                .or_default()
                .insert(callee.clone());
            g.callers.entry(callee).or_default().insert(caller);
            if inserted {
                g.edge_count += 1;
            }
        }
        Ok(g)
    }

    /// Legacy O(symbols × files) build. Retained for v1.0.0 indexes that
    /// predate the edges-at-extract pipeline.
    pub fn build_legacy(store: &Store, root: &Path) -> Result<Self> {
        let mut g = Self::default();
        for sym in store.iter_all_symbols()? {
            let hits = match query::find_callers(store, root, &sym.name) {
                Ok(h) => h,
                Err(_) => continue,
            };
            for h in hits {
                if let Some(caller) = enclosing_symbol_at(store, &h.file, h.line)? {
                    let inserted = g
                        .callees
                        .entry(caller.clone())
                        .or_default()
                        .insert(sym.name.clone());
                    g.callers
                        .entry(sym.name.clone())
                        .or_default()
                        .insert(caller);
                    if inserted {
                        g.edge_count += 1;
                    }
                }
            }
        }
        Ok(g)
    }

    /// BFS over outgoing edges starting at `name`.
    pub fn outgoing(&self, name: &str, depth: usize) -> Vec<GraphHit> {
        bfs(&self.callees, name, depth)
    }

    /// BFS over reverse edges (who calls `name`?).
    pub fn incoming(&self, name: &str, depth: usize) -> Vec<GraphHit> {
        bfs(&self.callers, name, depth)
    }

    /// Strongly connected components of size >= 2 (mutual recursion / cycles).
    /// Self-loops on a single node are NOT reported here — only the multi-node
    /// case, which is the interesting one for "is there a cycle through this
    /// codebase?". Returned components are sorted alphabetically; the outer
    /// list is sorted by first-element name for stable output.
    pub fn cycles(&self) -> Vec<Vec<String>> {
        let mut sccs = tarjan_scc(&self.callees);
        sccs.retain(|c| c.len() >= 2);
        for c in &mut sccs {
            c.sort();
        }
        sccs.sort_by(|a, b| a[0].cmp(&b[0]));
        sccs
    }

    /// Names that are in `callees` (i.e. they call something) but never appear
    /// as a callee. Subset of "uncalled top-level functions" — useful for
    /// dead-code triage. Symbols that only appear on the receiving end (entry
    /// points like main / handlers) are not included; you have to filter by
    /// the symbols table to add those.
    pub fn orphans(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .callees
            .keys()
            .filter(|k| !self.callers.contains_key(*k))
            .cloned()
            .collect();
        out.sort();
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
                    g.callers
                        .entry(callee.clone())
                        .or_default()
                        .insert(caller.clone());
                }
            }
        }
        Ok(g)
    }
}

fn bfs(adj: &BTreeMap<String, BTreeSet<String>>, start: &str, depth: usize) -> Vec<GraphHit> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut q: VecDeque<(String, usize)> = VecDeque::new();
    q.push_back((start.to_string(), 0));
    seen.insert(start.to_string());
    while let Some((node, d)) = q.pop_front() {
        if d > 0 {
            out.push(GraphHit {
                name: node.clone(),
                depth: d,
            });
        }
        if d >= depth {
            continue;
        }
        if let Some(neighbours) = adj.get(&node) {
            for n in neighbours {
                if seen.insert(n.clone()) {
                    q.push_back((n.clone(), d + 1));
                }
            }
        }
    }
    out
}

/// Tarjan's strongly-connected-components — iterative implementation to
/// avoid blowing the stack on deep call chains (the recursive version is
/// nicer to read but real-world graphs hit ~thousands deep on Rails monoliths).
fn tarjan_scc(adj: &BTreeMap<String, BTreeSet<String>>) -> Vec<Vec<String>> {
    let mut idx: BTreeMap<String, usize> = BTreeMap::new();
    let mut lowlink: BTreeMap<String, usize> = BTreeMap::new();
    let mut on_stack: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    let mut next_index: usize = 0;
    let mut sccs: Vec<Vec<String>> = Vec::new();

    // Each work-frame remembers which neighbour we're inspecting (resume index)
    // so we can drive the DFS iteratively without losing per-call state.
    enum Frame {
        Enter(String),
        Resume {
            node: String,
            iter_pos: usize,
            children: Vec<String>,
        },
    }

    let mut nodes: Vec<&String> = adj.keys().collect();
    nodes.sort();
    for start in nodes {
        if idx.contains_key(start) {
            continue;
        }
        let mut work: Vec<Frame> = vec![Frame::Enter(start.clone())];
        while let Some(frame) = work.pop() {
            match frame {
                Frame::Enter(node) => {
                    idx.insert(node.clone(), next_index);
                    lowlink.insert(node.clone(), next_index);
                    next_index += 1;
                    stack.push(node.clone());
                    on_stack.insert(node.clone());
                    let children: Vec<String> = adj
                        .get(&node)
                        .map(|s| s.iter().cloned().collect())
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
                        let child = &children[iter_pos];
                        iter_pos += 1;
                        if !idx.contains_key(child) {
                            // Push the resume frame back, then descend.
                            work.push(Frame::Resume {
                                node: node.clone(),
                                iter_pos,
                                children: children.clone(),
                            });
                            work.push(Frame::Enter(child.clone()));
                            descended = true;
                            break;
                        } else if on_stack.contains(child) {
                            let cl = lowlink[child];
                            let nl = lowlink[&node];
                            lowlink.insert(node.clone(), nl.min(cl));
                        }
                    }
                    if descended {
                        continue;
                    }
                    // Finished this node — propagate lowlink upward.
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
                        let parent = parent.clone();
                        lowlink.insert(parent, pl.min(cl));
                    }
                }
            }
        }
    }
    sccs
}

fn enclosing_symbol_at(store: &Store, file: &str, line: u32) -> Result<Option<String>> {
    // The smallest line_start <= line where line_end >= line, preferring methods
    // over their containing class. We approximate "smallest" by sorting by
    // (line_start desc, line_end asc) and taking the first match.
    let mut syms = store.symbols_in_file(file)?;
    syms.retain(|s| s.line_start <= line && s.line_end >= line);
    syms.sort_by(|a, b| {
        b.line_start
            .cmp(&a.line_start)
            .then(a.line_end.cmp(&b.line_end))
    });
    Ok(syms.into_iter().next().map(|s| s.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::build_index;
    use crate::store::Store;

    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("a.ts"),
            "export function low(){ return 1; }\n\
             export function mid(){ return low() + 1; }\n\
             export function high(){ return mid() + low(); }\n",
        )
        .unwrap();
        let store = Store::open(&root.join("idx.db")).unwrap();
        build_index(root, &store).unwrap();
        (dir, store)
    }

    #[test]
    fn graph_build_links_high_to_mid_and_low() {
        let (dir, store) = fixture();
        let g = CallGraph::build(&store, dir.path()).unwrap();
        // high() calls mid() and low(); mid() calls low().
        let from_high: BTreeSet<&String> = g
            .callees
            .get("high")
            .map(|s| s.iter().collect())
            .unwrap_or_default();
        assert!(
            from_high.iter().any(|n| n.as_str() == "mid"),
            "high → mid: {from_high:?}"
        );
        assert!(
            from_high.iter().any(|n| n.as_str() == "low"),
            "high → low: {from_high:?}"
        );

        let mid_callers: BTreeSet<&String> = g
            .callers
            .get("mid")
            .map(|s| s.iter().collect())
            .unwrap_or_default();
        assert!(mid_callers.iter().any(|n| n.as_str() == "high"));
    }

    #[test]
    fn outgoing_bfs_depth_bounded() {
        let (dir, store) = fixture();
        let g = CallGraph::build(&store, dir.path()).unwrap();
        let d1 = g.outgoing("high", 1);
        let d2 = g.outgoing("high", 2);
        assert!(!d1.is_empty(), "depth 1 should reach immediate callees");
        assert!(d2.len() >= d1.len(), "depth 2 must include ≥ depth 1");
    }

    #[test]
    fn save_and_load_round_trip() {
        let (dir, store) = fixture();
        let g = CallGraph::build(&store, dir.path()).unwrap();
        let path = dir.path().join("graph.json");
        g.save(&path).unwrap();
        let g2 = CallGraph::load(&path).unwrap();
        assert_eq!(g.callees, g2.callees);
        // Reverse callers are rebuilt on load even if omitted from disk.
        assert!(!g2.callers.is_empty());
    }

    #[test]
    fn unknown_symbol_returns_empty_bfs() {
        let mut g = CallGraph::default();
        g.callees
            .insert("a".into(), [String::from("b")].into_iter().collect());
        let hits = g.outgoing("does-not-exist", 5);
        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn cycle_is_traversed_without_infinite_loop() {
        let mut g = CallGraph::default();
        g.callees
            .insert("a".into(), [String::from("b")].into_iter().collect());
        g.callees
            .insert("b".into(), [String::from("a")].into_iter().collect());
        let hits = g.outgoing("a", 10);
        // Should hit b once and stop (we already saw a as the start).
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "b");
    }

    // ---- v2.0 paths ----

    #[test]
    fn build_from_edges_matches_legacy_shape() {
        let (dir, store) = fixture();
        // After build_index, edges_populated='1' so this is the SQL path.
        let from_edges = CallGraph::build(&store, dir.path()).unwrap();
        // Force the legacy walk for comparison.
        let from_legacy = CallGraph::build_legacy(&store, dir.path()).unwrap();

        // Both should connect high → mid and high → low.
        for graph in [&from_edges, &from_legacy] {
            let from_high: BTreeSet<&String> = graph
                .callees
                .get("high")
                .map(|s| s.iter().collect())
                .unwrap_or_default();
            assert!(from_high.iter().any(|n| n.as_str() == "mid"), "{graph:?}");
            assert!(from_high.iter().any(|n| n.as_str() == "low"), "{graph:?}");
        }
        // The SQL build is at least as complete as the legacy build (it sees
        // calls inside arrow functions / anonymous bodies that the legacy
        // path can also enumerate via ast-grep).
        assert!(from_edges.edge_count >= 3, "edges: {from_edges:?}");
    }

    #[test]
    fn cycles_returns_mutual_recursion_components() {
        let mut g = CallGraph::default();
        // a ↔ b mutual; c standalone.
        g.callees
            .insert("a".into(), [String::from("b")].into_iter().collect());
        g.callees
            .insert("b".into(), [String::from("a")].into_iter().collect());
        g.callees
            .insert("c".into(), [String::from("a")].into_iter().collect());
        let cycles = g.cycles();
        assert_eq!(cycles.len(), 1, "got: {cycles:?}");
        assert_eq!(cycles[0], vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn cycles_ignores_self_loops() {
        let mut g = CallGraph::default();
        g.callees
            .insert("rec".into(), [String::from("rec")].into_iter().collect());
        // Single-node SCC with a self-loop is interesting but we filter for
        // multi-node SCCs only — pure self-recursion isn't usually a smell.
        assert!(g.cycles().is_empty());
    }

    #[test]
    fn orphans_returns_callers_without_incoming_edges() {
        let mut g = CallGraph::default();
        // entry → mid → leaf
        g.callees
            .insert("entry".into(), [String::from("mid")].into_iter().collect());
        g.callers
            .insert("mid".into(), [String::from("entry")].into_iter().collect());
        g.callees
            .insert("mid".into(), [String::from("leaf")].into_iter().collect());
        g.callers
            .insert("leaf".into(), [String::from("mid")].into_iter().collect());
        // entry has callees but no callers → orphan candidate.
        let orphans = g.orphans();
        assert!(orphans.contains(&"entry".to_string()), "got: {orphans:?}");
        // mid has both → not an orphan.
        assert!(!orphans.contains(&"mid".to_string()));
    }

    /// Microbenchmark: legacy vs SQL build on a synthetic 50-symbol fixture.
    /// Run with `cargo test --release -- --ignored bench_graph_build_speedup`.
    /// Prints a table and asserts the SQL path is at least as fast as legacy
    /// (we expect 5–50× on real repos; the small fixture barely shows it).
    #[test]
    #[ignore = "perf microbench — run with --release --ignored"]
    fn bench_graph_build_speedup() {
        use std::time::Instant;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // 50 mutually-calling functions across 5 files. Each function calls
        // the next 3, so the legacy O(n²) is 50 × 50 ast-grep walks.
        for f in 0..5 {
            let mut body = String::new();
            for i in 0..10 {
                let name = f * 10 + i;
                let next = (name + 1) % 50;
                let next2 = (name + 2) % 50;
                let next3 = (name + 3) % 50;
                body.push_str(&format!(
                    "export function fn{name}(){{ return fn{next}() + fn{next2}() + fn{next3}(); }}\n"
                ));
            }
            std::fs::write(root.join(format!("a{f}.ts")), body).unwrap();
        }
        let store = Store::open(&root.join("idx.db")).unwrap();
        crate::index::full_index(root, &store).unwrap();

        let t0 = Instant::now();
        let g_sql = CallGraph::build_from_edges(&store).unwrap();
        let dt_sql = t0.elapsed();

        let t0 = Instant::now();
        let g_legacy = CallGraph::build_legacy(&store, root).unwrap();
        let dt_legacy = t0.elapsed();

        println!(
            "graph build  SQL: {:>6}µs  legacy: {:>6}µs  speedup: {:.1}×  edges: sql={} legacy={}",
            dt_sql.as_micros(),
            dt_legacy.as_micros(),
            dt_legacy.as_secs_f64() / dt_sql.as_secs_f64().max(1e-9),
            g_sql.edge_count,
            g_legacy.edge_count,
        );
        assert!(
            dt_sql <= dt_legacy,
            "SQL path should be at least as fast as legacy"
        );
    }
}
