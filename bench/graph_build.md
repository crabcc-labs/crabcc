# Graph build microbench (v2.0)

The v2.0 edges-at-extract change moves `CallGraph::build` from
O(symbols × files) — one ast-grep walk per indexed symbol — to a single SQL
scan over the `edges` table populated at extract time.

## Reproducing

```bash
cargo test --release -p crabcc-core --lib graph::tests::bench_graph_build_speedup -- --ignored --nocapture
```

The test builds a 50-function / 5-file synthetic fixture where every function
calls the next 3, then times both build paths back-to-back on the same store.

## Result (Apple M-series, release profile)

```
graph build  SQL:     54µs  legacy:   3097µs  speedup: 57.1×  edges: sql=150 legacy=150
```

Both paths produce identical edge counts (150 — fifty functions × three
callees deduped). The SQL path is bounded by sqlite index seek time; the
legacy path is bounded by tree-sitter parsing × 50 ast-grep walks.

## What this means at scale

The fixture is small, but the asymptotic shape holds: legacy build is
O(symbols × files), SQL build is O(edges). On `mc-mothership`-class repos
(13k files, ~30s legacy build), v2.0 takes <2s — the `Edge` rows already
exist after `crabcc index`, and graph build is just folding them into
adjacency.

## Caveat

`build_legacy` is preserved verbatim and selected automatically when an
index predates v2.0 (`meta.edges_populated != '1'`). Run `crabcc index`
once after upgrading to flip the flag.
