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
