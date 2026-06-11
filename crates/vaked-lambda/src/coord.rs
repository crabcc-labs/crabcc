//! Coordination IR for vaked-lambda: a timepoint dependency graph over units.
//!
//! A unit's `yields` timepoint is its own `id`, so the graph is a plain DAG
//! over `u32` ids. `coord_reduce` folds it to the minimal residual coordination
//! per target — the coordination analogue of value-level constant-folding.

use crate::Term;
use std::collections::{HashMap, HashSet};

/// An SSA value marking "this unit's work is done; results available".
/// By convention a unit's timepoint equals its `id`.
pub type Timepoint = u32;

/// When a unit's timepoint can resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    /// Resolved at build/compose time (e.g. a statically-composed mcconf module).
    Static,
    /// Resolved at boot (e.g. unikernel boot config).
    Boot,
    /// Resolved only at runtime (e.g. a distributed/fleet dependency).
    Runtime,
}

/// A schedulable unit: run `body` once all `awaits` resolve; completion is `yields`.
#[derive(Debug, Clone, PartialEq)]
pub struct Unit {
    pub id: u32,
    pub body: Term,
    pub awaits: Vec<Timepoint>,
    pub yields: Timepoint,
    pub kind: UnitKind,
}

/// A coordination program: units keyed by id (id == index is NOT assumed).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CoordGraph {
    pub units: Vec<Unit>,
}

/// A module as seen by the lowering: name, what it provides, what it requires.
/// Mirrors the mcconf-style descriptor (`provides`/`requires`).
pub struct ModuleSpec {
    pub name: String,
    pub provides: Vec<String>,
    pub requires: Vec<String>,
    pub body: Term,
    pub kind: UnitKind,
}

/// Lower a module dependency graph to a CoordGraph. Each module becomes a Unit
/// whose id/yields is its index; `requires` map to the ids of the units that
/// `provide` those names. Unknown requires are skipped (treated as external).
pub fn module_dag_to_coord(modules: Vec<ModuleSpec>) -> CoordGraph {
    let mut provider: HashMap<String, u32> = HashMap::new();
    for (i, m) in modules.iter().enumerate() {
        for p in &m.provides {
            provider.insert(p.clone(), i as u32);
        }
    }
    let units = modules
        .into_iter()
        .enumerate()
        .map(|(i, m)| {
            let mut awaits: Vec<Timepoint> = m
                .requires
                .iter()
                .filter_map(|r| provider.get(r).copied())
                .collect();
            awaits.sort_unstable();
            awaits.dedup();
            Unit {
                id: i as u32,
                body: m.body,
                awaits,
                yields: i as u32,
                kind: m.kind,
            }
        })
        .collect();
    CoordGraph { units }
}

/// Lower a Term's coordination nodes (Seq/Par/Dep) into a CoordGraph. Each
/// non-coordination operand becomes a Unit (its body). `Seq(a,b)` makes b await
/// a; `Dep(a,on)` makes a await on; `Par(a,b)` adds no edge between them.
/// Returns the graph; unit ids are assigned in post-order.
pub fn term_to_coord(term: &Term, kind: UnitKind) -> CoordGraph {
    let mut units: Vec<Unit> = Vec::new();
    // Lower a block, returning (entry_id, exit_id): the first unit to run and
    // the last to complete. Coordination edges attach entry-to-exit so nested
    // blocks chain correctly (e.g. Seq(a, Seq(b,c)) => a -> b -> c).
    fn go(t: &Term, kind: UnitKind, units: &mut Vec<Unit>) -> (u32, u32) {
        match t {
            Term::Seq(a, b) => {
                let (ea, xa) = go(a, kind, units);
                let (eb, xb) = go(b, kind, units);
                units[eb as usize].awaits.push(xa); // b's entry awaits a's exit
                (ea, xb)
            }
            Term::Dep(a, on) => {
                let (_eon, xon) = go(on, kind, units);
                let (ea, xa) = go(a, kind, units);
                units[ea as usize].awaits.push(xon); // a's entry awaits on's exit
                (ea, xa)
            }
            Term::Par(a, b) => {
                let (ea, _xa) = go(a, kind, units);
                let (_eb, xb) = go(b, kind, units);
                (ea, xb) // no edge; representative span entry(a)..exit(b)
            }
            leaf => {
                let id = units.len() as u32;
                units.push(Unit {
                    id,
                    body: leaf.clone(),
                    awaits: vec![],
                    yields: id,
                    kind,
                });
                (id, id)
            }
        }
    }
    go(term, kind, &mut units);
    for u in &mut units {
        u.awaits.sort_unstable();
        u.awaits.dedup();
    }
    CoordGraph { units }
}

/// Which units' timepoints resolve at build/compose time (and so need no
/// runtime coordination). A target picks the predicate.
pub struct TargetProfile {
    pub resolves_at_build: fn(&Unit) -> bool,
}

impl TargetProfile {
    /// Static-composition target (e.g. MyThOS mcconf): everything resolves at build.
    pub fn static_composition() -> Self {
        Self {
            resolves_at_build: |_| true,
        }
    }
    /// Runtime/fleet target: nothing resolves at build.
    pub fn runtime() -> Self {
        Self {
            resolves_at_build: |_| false,
        }
    }
    /// Boot target: only Static/Boot kinds resolve at build.
    pub fn boot() -> Self {
        Self {
            resolves_at_build: |u| matches!(u.kind, UnitKind::Static | UnitKind::Boot),
        }
    }
}

/// Set of ids whose timepoints resolve at build time under `profile`.
fn build_resolved(graph: &CoordGraph, profile: &TargetProfile) -> HashSet<u32> {
    graph
        .units
        .iter()
        .filter(|u| (profile.resolves_at_build)(u))
        .map(|u| u.id)
        .collect()
}

/// Step 1: remove build-time-resolved providers from every unit's awaits.
fn fold_build_time(mut graph: CoordGraph, profile: &TargetProfile) -> CoordGraph {
    let resolved = build_resolved(&graph, profile);
    for u in &mut graph.units {
        u.awaits.retain(|t| !resolved.contains(t));
    }
    graph
}

/// Build adjacency: unit id -> its direct awaits (after any folding).
fn adjacency(graph: &CoordGraph) -> HashMap<u32, Vec<u32>> {
    graph
        .units
        .iter()
        .map(|u| (u.id, u.awaits.clone()))
        .collect()
}

/// Is `target` reachable from `start` following awaits edges (excluding the
/// direct start->target edge itself)? Used to detect redundant awaits.
fn reachable_excluding_direct(adj: &HashMap<u32, Vec<u32>>, start: u32, target: u32) -> bool {
    let mut stack: Vec<u32> = adj
        .get(&start)
        .into_iter()
        .flatten()
        .copied()
        .filter(|&t| t != target) // ignore the direct edge we are testing
        .collect();
    let mut seen: HashSet<u32> = HashSet::new();
    while let Some(n) = stack.pop() {
        if n == target {
            return true;
        }
        if !seen.insert(n) {
            continue;
        }
        if let Some(next) = adj.get(&n) {
            stack.extend(next.iter().copied());
        }
    }
    false
}

/// Step 2: drop an await on `t` from unit `u` if `t` is already reachable from
/// `u` through another await (transitive reduction).
fn transitive_reduce(mut graph: CoordGraph) -> CoordGraph {
    let adj = adjacency(&graph);
    for u in &mut graph.units {
        let id = u.id;
        u.awaits
            .retain(|&t| !reachable_excluding_direct(&adj, id, t));
    }
    graph
}

/// How a surviving await edge is realized at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitKind {
    /// Provider has exactly one consumer -> point-to-point.
    PointToPoint,
    /// Provider has multiple consumers -> a barrier.
    Barrier,
}

/// Classify each surviving (consumer, provider) edge after reduction.
pub fn classify_waits(graph: &CoordGraph) -> Vec<(u32, u32, WaitKind)> {
    let mut consumers: HashMap<u32, u32> = HashMap::new(); // provider -> consumer count
    for u in &graph.units {
        for &t in &u.awaits {
            *consumers.entry(t).or_insert(0) += 1;
        }
    }
    let mut out = Vec::new();
    for u in &graph.units {
        for &t in &u.awaits {
            let kind = if consumers.get(&t).copied().unwrap_or(0) > 1 {
                WaitKind::Barrier
            } else {
                WaitKind::PointToPoint
            };
            out.push((u.id, t, kind));
        }
    }
    out
}

/// The full pass: build-time fold, then transitive reduction. Returns the
/// residual graph with minimized `awaits`. (Step 3 "dead-coordination" is
/// subsumed: an await to a build-folded provider is already removed, and a unit
/// with empty residual awaits is coordination-closed. Downgrade is reported by
/// `classify_waits` and consumed by `emit_coord`.)
pub fn coord_reduce(graph: CoordGraph, profile: &TargetProfile) -> CoordGraph {
    transitive_reduce(fold_build_time(graph, profile))
}

/// Topological order of unit ids (Kahn's algorithm over awaits edges).
fn topo_order(graph: &CoordGraph) -> Vec<u32> {
    let mut indeg: HashMap<u32, usize> =
        graph.units.iter().map(|u| (u.id, u.awaits.len())).collect();
    // provider -> consumers
    let mut consumers: HashMap<u32, Vec<u32>> = HashMap::new();
    for u in &graph.units {
        for &t in &u.awaits {
            consumers.entry(t).or_default().push(u.id);
        }
    }
    let mut ready: Vec<u32> = indeg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&id, _)| id)
        .collect();
    ready.sort_unstable();
    let mut out = Vec::new();
    while let Some(n) = ready.pop() {
        out.push(n);
        if let Some(cs) = consumers.get(&n) {
            for &c in cs {
                let d = indeg.get_mut(&c).unwrap();
                *d -= 1;
                if *d == 0 {
                    ready.push(c);
                }
            }
        }
        ready.sort_unstable();
    }
    out
}

/// Emit the residual coordination. Static-composition: a build order, no sync
/// primitives. Otherwise: one wait line per surviving edge (barrier / p2p).
pub fn emit_coord(graph: &CoordGraph, profile: &TargetProfile) -> String {
    let name = |id: u32| {
        graph
            .units
            .iter()
            .find(|u| u.id == id)
            .map(|u| match &u.body {
                Term::Lit(s) => s.clone(),
                _ => format!("unit{id}"),
            })
            .unwrap_or_else(|| format!("unit{id}"))
    };

    let any_residual = graph.units.iter().any(|u| !u.awaits.is_empty());
    if !any_residual {
        // coordination-closed: just a build order.
        let order: Vec<String> = topo_order(graph).into_iter().map(name).collect();
        return format!(
            "# build order (0 runtime barriers)\n{}\n",
            order.join(" -> ")
        );
    }
    let mut out = String::from("# residual coordination\n");
    for (consumer, provider, kind) in classify_waits(graph) {
        let tag = match kind {
            WaitKind::Barrier => "barrier",
            WaitKind::PointToPoint => "p2p",
        };
        out.push_str(&format!(
            "{} waits-on {} ({tag})\n",
            name(consumer),
            name(provider)
        ));
    }
    let _ = profile;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> Term {
        Term::Lit(s.to_string())
    }

    fn example_modules() -> Vec<ModuleSpec> {
        // base -> {mem, sched} -> net -> kernel  (the nushell recursive-build graph)
        vec![
            ModuleSpec {
                name: "base".into(),
                provides: vec!["base".into()],
                requires: vec![],
                body: lit("base"),
                kind: UnitKind::Static,
            },
            ModuleSpec {
                name: "mem".into(),
                provides: vec!["mem".into()],
                requires: vec!["base".into()],
                body: lit("mem"),
                kind: UnitKind::Static,
            },
            ModuleSpec {
                name: "sched".into(),
                provides: vec!["sched".into()],
                requires: vec!["base".into()],
                body: lit("sched"),
                kind: UnitKind::Static,
            },
            ModuleSpec {
                name: "net".into(),
                provides: vec!["net".into()],
                requires: vec!["mem".into()],
                body: lit("net"),
                kind: UnitKind::Static,
            },
            ModuleSpec {
                name: "kernel".into(),
                provides: vec!["kernel".into()],
                requires: vec!["mem".into(), "sched".into(), "net".into()],
                body: lit("kernel"),
                kind: UnitKind::Static,
            },
        ]
    }

    #[test]
    fn module_dag_lowers_to_expected_edges() {
        let g = module_dag_to_coord(example_modules());
        // ids: base=0, mem=1, sched=2, net=3, kernel=4
        assert_eq!(g.units[1].awaits, vec![0]); // mem -> base
        assert_eq!(g.units[2].awaits, vec![0]); // sched -> base
        assert_eq!(g.units[3].awaits, vec![1]); // net -> mem
        assert_eq!(g.units[4].awaits, vec![1, 2, 3]); // kernel -> mem, sched, net
    }

    #[test]
    fn downgrade_marks_shared_provider_as_barrier() {
        // base (id 0) has two consumers (mem, sched) under a runtime profile -> Barrier.
        // net (id 3) has one consumer (kernel) -> PointToPoint.
        let g = coord_reduce(
            module_dag_to_coord(example_modules()),
            &TargetProfile::runtime(),
        );
        let waits = classify_waits(&g);
        assert!(waits
            .iter()
            .any(|&(_, p, k)| p == 0 && k == WaitKind::Barrier));
        assert!(waits
            .iter()
            .any(|&(_, p, k)| p == 3 && k == WaitKind::PointToPoint));
    }

    #[test]
    fn transitive_reduction_drops_redundant_await() {
        // kernel awaits {mem, sched, net}; net awaits mem -> kernel->mem is redundant.
        let g = module_dag_to_coord(example_modules());
        let reduced = transitive_reduce(g);
        // kernel = id 4; mem = 1 should be pruned (reachable via net=3), leaving {2,3}.
        let kernel = reduced.units.iter().find(|u| u.id == 4).unwrap();
        assert_eq!(
            kernel.awaits,
            vec![2, 3],
            "kernel->mem elided (mem reachable via net)"
        );
    }

    #[test]
    fn static_profile_folds_all_awaits() {
        let g = module_dag_to_coord(example_modules());
        let folded = fold_build_time(g, &TargetProfile::static_composition());
        assert!(
            folded.units.iter().all(|u| u.awaits.is_empty()),
            "static composition resolves every timepoint at build -> zero residual awaits"
        );
    }

    #[test]
    fn term_to_coord_builds_seq_edge() {
        // Seq(a, Seq(b, c)) : c awaits b, b awaits a
        let t = Term::Seq(
            Box::new(lit("a")),
            Box::new(Term::Seq(Box::new(lit("b")), Box::new(lit("c")))),
        );
        let g = term_to_coord(&t, UnitKind::Runtime);
        assert_eq!(g.units.len(), 3);
        // post-order ids: a=0, b=1, c=2
        assert_eq!(g.units[0].awaits, vec![] as Vec<Timepoint>); // a
        assert_eq!(g.units[1].awaits, vec![0]); // b awaits a
        assert_eq!(g.units[2].awaits, vec![1]); // c awaits b
    }

    #[test]
    fn emit_static_is_build_order_no_barriers() {
        let g = coord_reduce(
            module_dag_to_coord(example_modules()),
            &TargetProfile::static_composition(),
        );
        let out = emit_coord(&g, &TargetProfile::static_composition());
        assert!(out.contains("0 runtime barriers"));
        assert!(!out.contains("(barrier)")); // no per-edge barrier tags in a static build order
        assert!(out.contains("base")); // build order names units
    }

    #[test]
    fn emit_runtime_lists_residual_waits() {
        let g = coord_reduce(
            module_dag_to_coord(example_modules()),
            &TargetProfile::runtime(),
        );
        let out = emit_coord(&g, &TargetProfile::runtime());
        assert!(out.contains("residual coordination"));
        assert!(out.contains("(barrier)")); // base is shared
    }

    #[test]
    fn spec_static_yields_zero_residual_timepoints() {
        let g = coord_reduce(
            module_dag_to_coord(example_modules()),
            &TargetProfile::static_composition(),
        );
        let residual: usize = g.units.iter().map(|u| u.awaits.len()).sum();
        assert_eq!(
            residual, 0,
            "static composition -> zero residual coordination"
        );
    }

    #[test]
    fn spec_runtime_residual_is_transitive_reduction() {
        // Property: no surviving await is implied by another (transitive reduction holds).
        let g = coord_reduce(
            module_dag_to_coord(example_modules()),
            &TargetProfile::runtime(),
        );
        let adj = adjacency(&g);
        for u in &g.units {
            for &t in &u.awaits {
                assert!(
                    !reachable_excluding_direct(&adj, u.id, t),
                    "await {}->{} is redundant; transitive reduction failed",
                    u.id,
                    t
                );
            }
        }
    }

    #[test]
    fn spec_dead_coordination_dropped() {
        // A unit whose yields has no consumer contributes no wait to anyone.
        let g = coord_reduce(
            module_dag_to_coord(example_modules()),
            &TargetProfile::runtime(),
        );
        // kernel (id 4) is a sink: no other unit awaits it.
        assert!(g.units.iter().all(|u| !u.awaits.contains(&4)));
    }

    #[test]
    fn unit_yields_its_own_id() {
        let u = Unit {
            id: 3,
            body: Term::Lit("x".into()),
            awaits: vec![1, 2],
            yields: 3,
            kind: UnitKind::Static,
        };
        assert_eq!(u.yields, u.id);
        let g = CoordGraph { units: vec![u] };
        assert_eq!(g.units.len(), 1);
    }
}
