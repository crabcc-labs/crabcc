//! Tiny force-directed layout. Pure compute, no gpui dependency —
//! takes a [`GraphSnapshot`] and returns each node's position in unit
//! coordinates `[0,1] × [0,1]` so the renderer can scale into any
//! canvas size without re-running the simulation.
//!
//! Algorithm is the standard d3-force trio:
//!   * Coulomb-style pairwise repulsion (O(N²) — fine up to ~500 nodes)
//!   * Hooke-style spring along edges (target length = `TARGET_DIST`)
//!   * Centring force toward the canvas midpoint
//!
//! Velocity is integrated with a fixed timestep + damping. After
//! `WARMUP_STEPS` ticks the layout is "frozen" — no animation, no
//! per-frame work. Mirrors the synchronous-warmup pattern the web
//! dashboard uses (lifted from `crates/crabcc-viz/web/src/components/Graph.tsx`).

use std::collections::HashMap;

use rayon::prelude::*;

use crate::api::types::GraphSnapshot;

const WARMUP_STEPS: usize = 200;
const TARGET_DIST: f32 = 0.10; // unit coords — ~10% of canvas
const REPEL_K: f32 = 0.0005;
const SPRING_K: f32 = 0.05;
const CENTER_K: f32 = 0.02;
const DAMPING: f32 = 0.85;

/// Below this node count the parallel pairwise-repulsion path is a
/// net loss — rayon's work-stealing setup costs more than the
/// arithmetic saved. Above it, the parallel version (which does ~2×
/// arithmetic to enable lock-free per-`i` accumulation) wins via
/// raw core count.
///
/// The threshold is an estimate, not measured. Re-tune once a
/// `crates/crabcc-desktop` bench harness lands (warp-speed audit
/// "Skipped tips → Tip 0").
const PARALLEL_THRESHOLD: usize = 250;

#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// Positions in unit coords. 1:1 with `GraphSnapshot::nodes`.
    pub positions: Vec<(f32, f32)>,
    /// Edges resolved to node indices. Lifted out of the snapshot so
    /// the renderer doesn't redo the id→index lookup every frame.
    pub edge_indices: Vec<(usize, usize)>,
}

pub fn run(snapshot: &GraphSnapshot) -> Layout {
    let n = snapshot.nodes.len();
    if n == 0 {
        return Layout::default();
    }

    // id → index lookup
    let mut id_to_idx: HashMap<&str, usize> = HashMap::with_capacity(n);
    for (i, node) in snapshot.nodes.iter().enumerate() {
        id_to_idx.insert(&node.id, i);
    }
    let edge_indices: Vec<(usize, usize)> = snapshot
        .edges
        .iter()
        .filter_map(|e| {
            Some((
                *id_to_idx.get(e.src.as_str())?,
                *id_to_idx.get(e.dst.as_str())?,
            ))
        })
        .filter(|(a, b)| a != b)
        .collect();

    // Initial placement on a circle — deterministic, reproducible.
    let mut pos = vec![(0.0_f32, 0.0_f32); n];
    let mut vel = vec![(0.0_f32, 0.0_f32); n];
    let r = 0.35;
    let n_f = n as f32;
    for (i, p) in pos.iter_mut().enumerate() {
        let angle = (i as f32) / n_f * std::f32::consts::TAU;
        *p = (0.5 + angle.cos() * r, 0.5 + angle.sin() * r);
    }

    let mut force = vec![(0.0_f32, 0.0_f32); n];

    for _step in 0..WARMUP_STEPS {
        // Reset force accumulators.
        for f in force.iter_mut() {
            *f = (0.0, 0.0);
        }

        // Pairwise repulsion. Two paths share the same physics:
        //   * Sequential (default for small N) uses the j>i symmetry
        //     trick to halve arithmetic at the cost of cross-index
        //     writes (`force[i]` and `force[j]` both touched per pair).
        //   * Parallel (N >= PARALLEL_THRESHOLD) recomputes both
        //     directions so every iteration writes only `force[i]`,
        //     which is what rayon's `par_iter_mut` exposes safely.
        //     2× arithmetic, no contention — wins above the threshold
        //     once we have ~3+ cores worth of work.
        if n < PARALLEL_THRESHOLD {
            for i in 0..n {
                for j in (i + 1)..n {
                    let dx = pos[j].0 - pos[i].0;
                    let dy = pos[j].1 - pos[i].1;
                    let d2 = dx * dx + dy * dy + 1e-6;
                    let inv_d = d2.sqrt().recip();
                    let f = REPEL_K / d2;
                    let fx = f * dx * inv_d;
                    let fy = f * dy * inv_d;
                    force[i].0 -= fx;
                    force[i].1 -= fy;
                    force[j].0 += fx;
                    force[j].1 += fy;
                }
            }
        } else {
            let pos_ref = pos.as_slice();
            force.par_iter_mut().enumerate().for_each(|(i, f_i)| {
                let (px_i, py_i) = pos_ref[i];
                let mut fx_acc = 0.0_f32;
                let mut fy_acc = 0.0_f32;
                for (j, &(px_j, py_j)) in pos_ref.iter().enumerate() {
                    if i == j {
                        continue;
                    }
                    let dx = px_j - px_i;
                    let dy = py_j - py_i;
                    let d2 = dx * dx + dy * dy + 1e-6;
                    let inv_d = d2.sqrt().recip();
                    let f = REPEL_K / d2;
                    fx_acc -= f * dx * inv_d;
                    fy_acc -= f * dy * inv_d;
                }
                f_i.0 += fx_acc;
                f_i.1 += fy_acc;
            });
        }

        // Spring along edges.
        for &(a, b) in &edge_indices {
            let dx = pos[b].0 - pos[a].0;
            let dy = pos[b].1 - pos[a].1;
            let d = (dx * dx + dy * dy).sqrt() + 1e-6;
            let displacement = d - TARGET_DIST;
            let fx = SPRING_K * displacement * dx / d;
            let fy = SPRING_K * displacement * dy / d;
            force[a].0 += fx;
            force[a].1 += fy;
            force[b].0 -= fx;
            force[b].1 -= fy;
        }

        // Centring pull.
        for i in 0..n {
            force[i].0 += (0.5 - pos[i].0) * CENTER_K;
            force[i].1 += (0.5 - pos[i].1) * CENTER_K;
        }

        // Integrate.
        for i in 0..n {
            vel[i].0 = (vel[i].0 + force[i].0) * DAMPING;
            vel[i].1 = (vel[i].1 + force[i].1) * DAMPING;
            pos[i].0 += vel[i].0;
            pos[i].1 += vel[i].1;
            // Soft clamp into [0.02, 0.98] so nodes never escape the
            // visible canvas — outside that range the centering pull
            // takes a few extra ticks to drag them back.
            pos[i].0 = pos[i].0.clamp(0.02, 0.98);
            pos[i].1 = pos[i].1.clamp(0.02, 0.98);
        }
    }

    Layout {
        positions: pos,
        edge_indices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{GraphEdge, GraphNode};

    fn snap(nodes: &[&str], edges: &[(&str, &str)]) -> GraphSnapshot {
        GraphSnapshot {
            nodes: nodes
                .iter()
                .map(|id| GraphNode {
                    id: id.to_string(),
                    depth: 0,
                })
                .collect(),
            edges: edges
                .iter()
                .map(|(s, d)| GraphEdge {
                    src: s.to_string(),
                    dst: d.to_string(),
                })
                .collect(),
            seeds: vec![],
        }
    }

    #[test]
    fn empty_input_produces_empty_layout() {
        let l = run(&snap(&[], &[]));
        assert!(l.positions.is_empty());
        assert!(l.edge_indices.is_empty());
    }

    #[test]
    fn positions_stay_in_unit_square() {
        let l = run(&snap(
            &["a", "b", "c", "d", "e"],
            &[("a", "b"), ("b", "c"), ("c", "d"), ("d", "e"), ("e", "a")],
        ));
        for (x, y) in &l.positions {
            assert!(*x >= 0.02 && *x <= 0.98, "x out of bounds: {x}");
            assert!(*y >= 0.02 && *y <= 0.98, "y out of bounds: {y}");
        }
    }

    #[test]
    fn unknown_edge_endpoints_are_dropped() {
        let l = run(&snap(
            &["a", "b"],
            &[("a", "b"), ("a", "ghost"), ("c", "d")],
        ));
        // Only ("a", "b") survives since ghost / c / d aren't in nodes.
        assert_eq!(l.edge_indices.len(), 1);
        assert_eq!(l.edge_indices[0], (0, 1));
    }

    #[test]
    fn self_loops_stripped() {
        let l = run(&snap(&["a", "b"], &[("a", "a"), ("a", "b")]));
        assert_eq!(l.edge_indices.len(), 1);
        assert_eq!(l.edge_indices[0], (0, 1));
    }

    #[test]
    fn parallel_path_stays_in_unit_square_at_threshold() {
        // Exercise the rayon branch: N == PARALLEL_THRESHOLD is the
        // smallest size that goes through `par_iter_mut`. We don't
        // assert numerical equivalence with the sequential path
        // (the algorithms differ — the parallel one recomputes both
        // directions, so floating-point ordering is different), only
        // that the same physics invariants hold: positions inside the
        // canvas, no NaN, no escape.
        let n = PARALLEL_THRESHOLD;
        let ids: Vec<String> = (0..n).map(|i| format!("n{i}")).collect();
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        // Sparse ring of edges so the spring force has work to do.
        let edge_pairs: Vec<(&str, &str)> =
            (0..n).map(|i| (id_refs[i], id_refs[(i + 1) % n])).collect();
        let l = run(&snap(&id_refs, &edge_pairs));
        assert_eq!(l.positions.len(), n);
        for (i, (x, y)) in l.positions.iter().enumerate() {
            assert!(x.is_finite(), "non-finite x at node {i}: {x}");
            assert!(y.is_finite(), "non-finite y at node {i}: {y}");
            assert!(*x >= 0.02 && *x <= 0.98, "x out of bounds at {i}: {x}");
            assert!(*y >= 0.02 && *y <= 0.98, "y out of bounds at {i}: {y}");
        }
    }
}
