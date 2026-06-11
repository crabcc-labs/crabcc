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

#[cfg(test)]
mod tests {
    use super::*;

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
