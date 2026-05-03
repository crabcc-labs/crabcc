//! `graph_layout::run` force-directed layout bench. Triggered once per
//! `GraphSnapshot` identity change (node-set fingerprint), so latency
//! here gates the time-to-first-frame on a fresh seed-graph fetch.
//! Two synthetic graph sizes — one small (the typical crabcc seed-
//! graph) and one large (stress, exercises the parallel threshold
//! covered by PR #282).

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};

use crabcc_desktop::api::types::{GraphEdge, GraphNode, GraphSnapshot};
use crabcc_desktop::graph_layout;

fn synth_graph(nodes: usize, edges_per_node: usize) -> GraphSnapshot {
    let nodes_vec: Vec<GraphNode> = (0..nodes)
        .map(|i| GraphNode {
            id: format!("n{i:04}"),
            depth: (i % 5) as u32,
        })
        .collect();
    let mut edges_vec: Vec<GraphEdge> = Vec::with_capacity(nodes * edges_per_node);
    // Wrap-around chain plus a few cross-links per node — gives the
    // force-directed solver a non-trivial graph to spread out without
    // requiring an importer.
    for i in 0..nodes {
        for k in 1..=edges_per_node {
            let j = (i + k) % nodes;
            if i != j {
                edges_vec.push(GraphEdge {
                    src: format!("n{i:04}"),
                    dst: format!("n{j:04}"),
                });
            }
        }
    }
    let seeds = nodes_vec.iter().take(3).map(|n| n.id.clone()).collect();
    GraphSnapshot {
        nodes: nodes_vec,
        edges: edges_vec,
        seeds,
    }
}

fn bench_layout_small(c: &mut Criterion) {
    // 50 nodes is the upper end of what crabcc's seed-graph emits in
    // practice on a typical repo — most are < 30.
    let snap = synth_graph(50, 3);
    c.bench_function("graph_layout_50_nodes", |b| {
        b.iter_batched(
            || snap.clone(),
            |snap| {
                let layout = graph_layout::run(&snap);
                black_box(layout);
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_layout_large(c: &mut Criterion) {
    // 500 nodes — stress fixture exercising the parallel-threshold
    // path (#282). Won't be hit in practice today but lets the bench
    // catch regressions on the parallel implementation.
    let snap = synth_graph(500, 4);
    c.bench_function("graph_layout_500_nodes", |b| {
        b.iter_batched(
            || snap.clone(),
            |snap| {
                let layout = graph_layout::run(&snap);
                black_box(layout);
            },
            BatchSize::LargeInput,
        );
    });
}

criterion_group!(benches, bench_layout_small, bench_layout_large);
criterion_main!(benches);
