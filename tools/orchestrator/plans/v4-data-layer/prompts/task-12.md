# Task 12 — CLI wiring + graph.rs upgrade to symbol-IDs

## Context

Wave 2c integrator. By the time this task fires, the v2b sub-wave has landed
four KG-op modules under `crates/crabcc-core/src/query/`:

```
crates/crabcc-core/src/query/
├── blast_radius.rs        (Task 8)
├── why.rs                 (Task 9)
├── hot_symbols.rs         (Task 10)
└── importers.rs           (Task 11)
```

Task 8 was also responsible for adding the four `pub mod` declarations to
`crates/crabcc-core/src/query/mod.rs`. This task **does not modify** that file
and **fails loudly** if those declarations are missing.

This task does two things:

1. **CLI surface** — `crates/crabcc-cli/src/main.rs`. Add four new variants to
   the existing `GraphOp` subcommand enum (`BlastRadius`, `Why`, `HotSymbols`,
   `Importers`) and four matching dispatch arms in the `Cmd::Graph` match.
2. **graph.rs upgrade** — `crates/crabcc-core/src/graph.rs`. The existing
   `CallGraph::build_from_edges`, `incoming`, `outgoing`, `cycles`, `orphans`
   are all keyed by `String` symbol names. v4 keys edges by `symbol_id` (i64).
   Rewrite the public surface so adjacency maps are `BTreeMap<i64,
   BTreeSet<i64>>` and the BFS / Tarjan / orphans logic operates on IDs. The
   on-the-fly JSON output resolves IDs back to qualified-or-bare names by
   asking the Store. The legacy v1.0.0 `build_legacy` walker is removed —
   v4 indexes always populate the `edges` table; the fallback no longer has
   a schema to match.

The function signatures in `graph.rs` change. The dispatch arms in `main.rs`
that call them must be updated in lock-step. Both files are in this task's
allow-list precisely because the change crosses both files.

## Pre-flight check (DO THIS FIRST)

Before touching any file, verify that the Wave 2b integrator step landed:

```bash
grep -q '^pub mod blast_radius;' crates/crabcc-core/src/query/mod.rs && \
grep -q '^pub mod why;'          crates/crabcc-core/src/query/mod.rs && \
grep -q '^pub mod hot_symbols;'  crates/crabcc-core/src/query/mod.rs && \
grep -q '^pub mod importers;'    crates/crabcc-core/src/query/mod.rs || \
  { echo "task-12 FAIL: Wave 2b integrator did not register the four KG-op modules in crates/crabcc-core/src/query/mod.rs" >&2; exit 1; }
```

Run that as your very first action. If it exits non-zero, stop the task — do
not touch any file, do not commit. The error is upstream (Task 8 did not run
or did not complete its mod-registration responsibility) and this task cannot
proceed safely.

## What to change

### File 1: `crates/crabcc-core/src/graph.rs`

Replace the entire file contents with:

```rust
//! Knowledge-base graph sidecar — caller/callee relationships over
//! symbol-IDs (v4).
//!
//! - v4 keys adjacency by `symbol_id` (i64), not by symbol name. This
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

use crate::store::Store;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::Path;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CallGraph {
    /// Outgoing: caller symbol_id -> set of callee symbol_ids.
    pub callees: BTreeMap<i64, BTreeSet<i64>>,
    /// Reverse: callee symbol_id -> set of caller symbol_ids (computed on load).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub callers: BTreeMap<i64, BTreeSet<i64>>,
    /// Total number of edges. For sanity checks + reporting.
    #[serde(default)]
    pub edge_count: usize,
}

#[derive(Debug, Serialize)]
pub struct GraphHit {
    pub symbol_id: i64,
    pub depth: usize,
}

impl CallGraph {
    /// Build the call graph from the v4 `edges` table. One SQL scan of
    /// `(src_symbol_id, dst_symbol_id)` pairs; no per-file walks, no
    /// parser invocations.
    pub fn build(store: &Store, _root: &Path) -> Result<Self> {
        let t0 = std::time::Instant::now();
        let mut g = Self::default();
        for (src, dst) in store.iter_call_edges_resolved()? {
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
    pub fn outgoing(&self, start_id: i64, depth: usize) -> Vec<GraphHit> {
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
    pub fn incoming(&self, start_id: i64, depth: usize) -> Vec<GraphHit> {
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
    pub fn cycles(&self) -> Vec<Vec<i64>> {
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
    pub fn orphans(&self) -> Vec<i64> {
        let t0 = std::time::Instant::now();
        let mut out: Vec<i64> = self
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

fn bfs(adj: &BTreeMap<i64, BTreeSet<i64>>, start: i64, depth: usize) -> Vec<GraphHit> {
    let mut out = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    let mut q: VecDeque<(i64, usize)> = VecDeque::new();
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
fn tarjan_scc(adj: &BTreeMap<i64, BTreeSet<i64>>) -> Vec<Vec<i64>> {
    let mut idx: BTreeMap<i64, usize> = BTreeMap::new();
    let mut lowlink: BTreeMap<i64, usize> = BTreeMap::new();
    let mut on_stack: HashSet<i64> = HashSet::new();
    let mut stack: Vec<i64> = Vec::new();
    let mut next_index: usize = 0;
    let mut sccs: Vec<Vec<i64>> = Vec::new();

    enum Frame {
        Enter(i64),
        Resume {
            node: i64,
            iter_pos: usize,
            children: Vec<i64>,
        },
    }

    let mut nodes: Vec<i64> = adj.keys().copied().collect();
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
                    let children: Vec<i64> = adj
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
```

This file replacement drops the v1.0.0 `build_legacy` path (and its
dependency on `crate::query` + `enclosing_symbol_at`), and drops the inline
unit tests that exercised the legacy name-keyed surface. The v4 surface gets
its end-to-end coverage from the integration test in Task 15 — keeping a
unit-test block in this file would need a fresh fixture that talks to the
new symbol-ID `edges` shape, which is out of scope for this task.

`Store::iter_call_edges_resolved` is the v4 replacement for
`Store::iter_call_edges`. It is added by Task 2 (the v4 Store API task) and
yields `(src_symbol_id, dst_symbol_id)` pairs. If you grep the Task 2 commit
and `iter_call_edges_resolved` isn't there, stop and fail the task — the
upstream contract was not met.

### File 2: `crates/crabcc-cli/src/main.rs`

This file has two edit sites: the `GraphOp` enum definition (around line
638) and the `Cmd::Graph { op } => match op { ... }` dispatch block (around
line 1343). Make both edits surgically. **Do not touch any other part of
this 2293-line file.**

#### Edit site A — `enum GraphOp` (around line 638)

Find this exact block:

```rust
#[derive(Subcommand)]
enum GraphOp {
    /// Rebuild the call-graph sidecar (.crabcc/graph.json) from the index.
    Build,
    /// BFS expansion: who calls / what does this symbol call?
    Walk {
        name: String,
        /// Direction: 'callers' (default) walks upward; 'callees' walks downward.
        #[arg(long, default_value = "callers")]
        dir: String,
        /// BFS depth limit.
        #[arg(long, default_value_t = 2)]
        depth: usize,
    },
    /// Find cycles: strongly-connected components of size ≥2 (mutual recursion).
    Cycles,
    /// List orphans: symbols that call others but have no incoming callers
    /// in the indexed graph. Useful as a dead-code triage starting point.
    Orphans,
}
```

Replace it with:

```rust
#[derive(Subcommand)]
enum GraphOp {
    /// Rebuild the call-graph sidecar (.crabcc/graph.json) from the index.
    Build,
    /// BFS expansion: who calls / what does this symbol call?
    Walk {
        name: String,
        /// Direction: 'callers' (default) walks upward; 'callees' walks downward.
        #[arg(long, default_value = "callers")]
        dir: String,
        /// BFS depth limit.
        #[arg(long, default_value_t = 2)]
        depth: usize,
    },
    /// Find cycles: strongly-connected components of size ≥2 (mutual recursion).
    Cycles,
    /// List orphans: symbols that call others but have no incoming callers
    /// in the indexed graph. Useful as a dead-code triage starting point.
    Orphans,
    /// Reverse transitive closure: everything that transitively depends on `symbol`.
    BlastRadius {
        symbol: String,
        /// Walk depth cap. None = walk to fixpoint.
        #[arg(long)]
        depth: Option<usize>,
        /// Edge-kind filter: 'call', 'ref', or 'all' (default).
        #[arg(long)]
        kind: Option<String>,
    },
    /// Shortest call-graph path from `src` to `dst` (bidirectional BFS).
    Why {
        src: String,
        dst: String,
        #[arg(long)]
        max_depth: Option<usize>,
    },
    /// Symbols ranked by in-degree (most-called first).
    HotSymbols {
        #[arg(long)]
        top: Option<usize>,
        /// Edge-kind filter: 'call', 'ref', or 'all' (default).
        #[arg(long)]
        kind: Option<String>,
    },
    /// File-level edge rollup: which files import (transitively reference) `path`.
    Importers {
        path: String,
        #[arg(long)]
        depth: Option<usize>,
    },
}
```

#### Edit site B — `Cmd::Graph` dispatch (around line 1343)

The current `Cmd::Graph { op } => match op { ... }` block (lines ~1342–1417)
calls `g.outgoing(&name, depth)` / `g.incoming(&name, depth)` — both take
`&str`. After the graph.rs upgrade above those signatures are `i64`. The
existing `Walk` arm must be updated, and four new arms appended.

Find this exact block:

```rust
        // ── Graph group ─────────────────────────────────────────────────────
        Cmd::Graph { op } => match op {
            GraphOp::Build => {
                let g = crabcc_core::graph::CallGraph::build(&store, &root)?;
                let path = resolved.graph_json();
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                g.save(&path)?;
                println!(
                    "{}",
                    sonic_rs::to_string(&serde_json::json!({
                        "edges":   g.edge_count,
                        "callers": g.callers.len(),
                        "callees": g.callees.len(),
                        "path":    path.to_string_lossy(),
                    }))?
                );
            }
            GraphOp::Walk {
                name,
                dir: direction,
                depth,
            } => {
                let path = resolved.graph_json();
                let g = if path.exists() {
                    crabcc_core::graph::CallGraph::load(&path)?
                } else {
                    eprintln!(
                        "crabcc graph walk: no graph.json at {} — building on the fly \
                         (run `crabcc graph build` to cache)",
                        path.display()
                    );
                    crabcc_core::graph::CallGraph::build(&store, &root)?
                };
                let hits = match direction.as_str() {
                    "callees" => g.outgoing(&name, depth),
                    _ => g.incoming(&name, depth),
                };
                let body = sonic_rs::to_string(&hits)?;
                crabcc_core::track::record(
                    "graph",
                    &name,
                    hits.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
            GraphOp::Cycles => {
                let g = load_or_build_graph(&store, &root, &resolved.graph_json())?;
                let cycles = g.cycles();
                let body = sonic_rs::to_string(&cycles)?;
                crabcc_core::track::record(
                    "graph-cycles",
                    "cycles",
                    cycles.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
            GraphOp::Orphans => {
                let g = load_or_build_graph(&store, &root, &resolved.graph_json())?;
                let orphans = g.orphans();
                let body = sonic_rs::to_string(&orphans)?;
                crabcc_core::track::record(
                    "graph-orphans",
                    "orphans",
                    orphans.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
        },
```

Replace it with:

```rust
        // ── Graph group ─────────────────────────────────────────────────────
        Cmd::Graph { op } => match op {
            GraphOp::Build => {
                let g = crabcc_core::graph::CallGraph::build(&store, &root)?;
                let path = resolved.graph_json();
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                g.save(&path)?;
                println!(
                    "{}",
                    sonic_rs::to_string(&serde_json::json!({
                        "edges":   g.edge_count,
                        "callers": g.callers.len(),
                        "callees": g.callees.len(),
                        "path":    path.to_string_lossy(),
                    }))?
                );
            }
            GraphOp::Walk {
                name,
                dir: direction,
                depth,
            } => {
                let symbol_id = resolve_symbol_id(&store, &name)?;
                let path = resolved.graph_json();
                let g = if path.exists() {
                    crabcc_core::graph::CallGraph::load(&path)?
                } else {
                    eprintln!(
                        "crabcc graph walk: no graph.json at {} — building on the fly \
                         (run `crabcc graph build` to cache)",
                        path.display()
                    );
                    crabcc_core::graph::CallGraph::build(&store, &root)?
                };
                let hits = match direction.as_str() {
                    "callees" => g.outgoing(symbol_id, depth),
                    _ => g.incoming(symbol_id, depth),
                };
                let body = sonic_rs::to_string(&hits)?;
                crabcc_core::track::record(
                    "graph",
                    &name,
                    hits.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
            GraphOp::Cycles => {
                let g = load_or_build_graph(&store, &root, &resolved.graph_json())?;
                let cycles = g.cycles();
                let body = sonic_rs::to_string(&cycles)?;
                crabcc_core::track::record(
                    "graph-cycles",
                    "cycles",
                    cycles.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
            GraphOp::Orphans => {
                let g = load_or_build_graph(&store, &root, &resolved.graph_json())?;
                let orphans = g.orphans();
                let body = sonic_rs::to_string(&orphans)?;
                crabcc_core::track::record(
                    "graph-orphans",
                    "orphans",
                    orphans.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
            GraphOp::BlastRadius {
                symbol,
                depth,
                kind,
            } => {
                let symbol_id = resolve_symbol_id(&store, &symbol)?;
                let hits = crabcc_core::query::blast_radius::blast_radius(
                    &store,
                    symbol_id,
                    depth,
                    kind.as_deref(),
                )?;
                let body = sonic_rs::to_string(&hits)?;
                crabcc_core::track::record(
                    "graph-blast-radius",
                    &symbol,
                    hits.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
            GraphOp::Why {
                src,
                dst,
                max_depth,
            } => {
                let src_id = resolve_symbol_id(&store, &src)?;
                let dst_id = resolve_symbol_id(&store, &dst)?;
                let path =
                    crabcc_core::query::why::why(&store, src_id, dst_id, max_depth)?;
                let body = sonic_rs::to_string(&path)?;
                crabcc_core::track::record(
                    "graph-why",
                    &format!("{src}->{dst}"),
                    path.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
            GraphOp::HotSymbols { top, kind } => {
                let hits = crabcc_core::query::hot_symbols::hot_symbols(
                    &store,
                    top,
                    kind.as_deref(),
                )?;
                let body = sonic_rs::to_string(&hits)?;
                crabcc_core::track::record(
                    "graph-hot-symbols",
                    "hot",
                    hits.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
            GraphOp::Importers { path, depth } => {
                let hits =
                    crabcc_core::query::importers::importers(&store, &path, depth)?;
                let body = sonic_rs::to_string(&hits)?;
                crabcc_core::track::record(
                    "graph-importers",
                    &path,
                    hits.len(),
                    &repo_label(&root),
                    body.len(),
                );
                println!("{body}");
            }
        },
```

#### Edit site C — add the `resolve_symbol_id` helper

The four new dispatch arms (plus the upgraded `Walk` arm) need to turn the
user-typed name `"Store::open"` into a `symbol_id` to feed the query
modules. Add this helper function at the bottom of `main.rs`, just before
the final closing brace of the file. Append it as the last item in the
file:

```rust

/// Resolve a user-typed symbol name to a `symbols.id`. Uses
/// `Store::find_by_name` (which already powers `lookup sym`); takes the first
/// hit and prints a stderr note when there are multiple candidates so the
/// caller knows the lookup was ambiguous.
fn resolve_symbol_id(store: &crabcc_core::store::Store, name: &str) -> anyhow::Result<i64> {
    let hits = store.find_by_name(name)?;
    if hits.is_empty() {
        anyhow::bail!("symbol not found: {name}");
    }
    if hits.len() > 1 {
        eprintln!(
            "crabcc: ambiguous symbol `{name}` ({n} candidates); using first hit at {file}:{line}",
            n = hits.len(),
            file = hits[0].file,
            line = hits[0].line_start,
        );
    }
    // `Symbol` doesn't expose `id` today — round-trip through the store.
    store.symbol_id_by_name_file(name, &hits[0].file, hits[0].line_start)
}
```

`store.symbol_id_by_name_file(name, file_path, line_start)` is a new
single-purpose accessor on `Store`. It is added by Task 2 (the v4 Store API
task). If you grep the Task 2 commit and `symbol_id_by_name_file` isn't
there, stop and fail the task — the upstream contract was not met.

## Definition of done

- `crates/crabcc-core/src/graph.rs` matches the replacement above
  verbatim.
- `crates/crabcc-cli/src/main.rs` has the new `GraphOp` enum with four
  added variants, the updated `Cmd::Graph` dispatch block with four added
  arms + the updated `Walk` arm, and the `resolve_symbol_id` helper at
  end-of-file.
- No other file in either crate is touched.
- The pre-flight grep against `query/mod.rs` was run and passed.

Do not run `cargo build`, `cargo test`, or any other build or test command.

Do not modify any other file. Do not invent extra files.

Then commit with this exact message:

    feat(cli): wire blast-radius/why/hot-symbols/importers; upgrade graph walk/cycles/orphans to symbol-IDs
