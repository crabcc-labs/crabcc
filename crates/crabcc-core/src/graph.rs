//! Knowledge-base graph sidecar — caller/callee relationships.
//!
//! v0.1 design (intentionally simple, intentionally cheap to maintain):
//!
//! - The graph is an adjacency map `caller -> {callee, …}` keyed by symbol
//!   name. Names are unqualified — we don't resolve receiver/module yet, so
//!   `Foo.bar()` and `Bar.bar()` collapse to `bar`. This is deliberate; agents
//!   asking "what calls bar?" usually want both.
//! - Built lazily by walking every indexed symbol, finding its enclosing
//!   parent, and emitting an edge. Slower than extraction-time but doesn't
//!   require a schema migration.
//! - Persisted to `.crabcc/graph.json`. `crabcc graph-build` rebuilds; `crabcc
//!   graph <name>` loads + queries. Falls back to live BFS if no cache exists.
//! - BFS expansion is depth-bounded; hash-set dedup avoids cycles.
//!
//! Polish targets for v0.2 (TODO):
//!   * Populate at extraction time using the `edges` table — drops build to
//!     O(n) instead of O(n²).
//!   * Resolve receivers (Ruby `Mod::Klass.foo` → `Klass.foo`).

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
    /// Build by scanning every symbol and finding its callers via the existing
    /// `query::find_callers` machinery. O(symbols × files) — slow on huge
    /// repos. The caller is expected to invoke `save()` and reuse via `load()`.
    pub fn build(store: &Store, root: &Path) -> Result<Self> {
        let mut g = Self::default();
        for sym in store.iter_all_symbols()? {
            let hits = match query::find_callers(store, root, &sym.name) {
                Ok(h) => h,
                Err(_) => continue, // bad-pattern names already filtered upstream
            };
            for h in hits {
                if let Some(caller) = enclosing_symbol_at(store, &h.file, h.line)? {
                    g.callees
                        .entry(caller.clone())
                        .or_default()
                        .insert(sym.name.clone());
                    g.callers
                        .entry(sym.name.clone())
                        .or_default()
                        .insert(caller);
                    g.edge_count += 1;
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
}
