# Internal Agent — crabcc-core specialist

You own `crates/crabcc-core/`. Read `internal_agents/shared.agent.md`
first — that's the workflow contract. This file is the crate-specific
context.

## What this crate does

`crabcc-core` is the **library** at the bottom of the stack. It owns:

- **Indexing pipeline** — tree-sitter symbol extraction, walker, store
  upsert path. See `extract.rs`, `index.rs`, `walker.rs`.
- **Storage** — SQLite Store with WAL, FSST compression codec, additive
  schema migrations. See `store.rs`, `schema/001_init.sql`.
- **Query primitives** — `sym`, `refs`, `callers`, `outline`. The
  user-facing CLI / MCP surface is just an adapter on top of these.
- **Call graph** — `graph.rs` (loader, walker, cycle detection, orphans).

## Hot paths

Performance-sensitive code lives here. Typical optimization rules:

- Pre-allocate `Vec::with_capacity(...)` whenever the size is known
  (extract walks, store batched upserts, graph BFS frontiers).
- `ahash::AHashMap` over `std::HashMap` for trusted-input keys
  (graph edges, symbol id → row mapping).
- `bumpalo::Bump` for transient per-file allocations during the
  tree-sitter walk.
- Profile before adding `std::arch` SIMD intrinsics. Tree-sitter does
  its own SIMD-aware parsing in C; the byte-scan loops downstream of
  it are the actual candidates.

## Bench gates that must stay green

- `task bench-compress` — FSST off-vs-on (you can NEVER regress this
  without explicitly accepting the trade-off in the PR description).
- `task bench` — raw-CLI A/B vs grep/find. Write-up lives in
  `bench/results/REPORT.md` (gitignored; the data itself isn't shipped).

## Cross-crate consumers — break these and CI cries

- `crabcc-cli` calls `Store`, `query::*`, `extract::*`, `graph::*`
- `crabcc-mcp` calls every public symbol on the query side
- `crabcc-viz` calls `graph::*` for the live dashboard
- `crabcc-memory` does NOT depend on core (memory is its own crate)

When your changes touch a public API: grep for callers across the
workspace BEFORE editing. The crabcc MCP `callers` tool is exactly
the right shape for this.
