# The crabcc call-graph sidecar (`.crabcc/graph.json`)

> Companion doc to [ARCHITECTURE.md](./ARCHITECTURE.md). Focuses on the
> call-graph sidecar — what it is, when it's built, how it's stored, and
> how the rest of crabcc consumes it.

## What it is

A serialized [`CallGraph`] snapshot at `<repo>/.crabcc/graph.json`. The
graph captures the directed `caller → callee` relationships extracted
from the symbol index. It is **not** the source of truth — that's the
`edges` table in `.crabcc/index.db`. The sidecar is a derived view kept
on disk so reads don't pay an aggregation cost on every `crabcc graph
walk` / `crabcc graph cycles` / `crabcc graph orphans` invocation.

## When it's built

Three triggers, in order of frequency:

| Trigger | Path | Notes |
|---|---|---|
| `crabcc graph build` | [`graph::CallGraph::build`] → `.save()` | Explicit user command. Always rebuilds from the live `edges` table. |
| `crabcc go` | [`crabcc_cli::go::init`] | Built unconditionally as part of the one-shot bootstrap (after indexing, before the Claude hand-off). |
| `crabcc graph walk NAME` | [`load_or_build_graph`] | If `.crabcc/graph.json` is missing, it's built on-demand and saved before the walk. |

`crabcc index` does **not** rebuild the sidecar by itself — it
populates the `edges` table and sets `meta.edges_populated = '1'` so
that future `graph build` calls take the fast SQL path.

## On-disk shape

JSON, Serde-serialized. Schema (canonical Rust definition lives in
`crates/crabcc-core/src/graph.rs`):

```rust
pub struct CallGraph {
    /// Outgoing: caller symbol name -> set of callee symbol names.
    pub callees: BTreeMap<String, BTreeSet<String>>,
    /// Reverse: callee name -> set of callers (computed on load).
    pub callers: BTreeMap<String, BTreeSet<String>>,
    /// Total number of edges. For sanity checks + reporting.
    pub edge_count: usize,
}
```

Both maps are `BTreeMap<String, BTreeSet<String>>` — sorted, set-typed.
That gives stable JSON output (diff-friendly, byte-reproducible) and
O(log n) symbol-name lookup on load. The `callers` field is reverse-
computed during deserialization so the sidecar doesn't carry duplicate
information; the field is `serialize_if_not_empty` in practice.

A typical sidecar on a 13k-file Rails repo is ~3–6 MB. The full
`crabcc graph build` round-trip on the same fixture is ~4 ms (single
SQL scan + BTreeMap inserts) — see the
`bench_graph_build_speedup` micro-bench for legacy-vs-SQL numbers.

## Build paths

[`CallGraph::build`] dispatches on the `meta.edges_populated` flag set
by `crabcc index`:

- `'1'` → [`build_from_edges`] — pure SQL: one `SELECT … FROM edges`
  scan plus BTreeMap inserts. ~9 ms on the 13k-file fixture.
- not set → [`build_legacy`] — the v1.x walker. Walks every symbol ×
  every file with ast-grep. Kept verbatim as a fallback so v1.x indexes
  upgrade losslessly: queries fall back to the slow path until the next
  full reindex flips the flag.

`save()` writes pretty-printed JSON via `serde_json::to_writer_pretty`.
`load()` is the symmetric `from_reader`. The reverse-`callers` map is
populated in `from_reader` because we don't want stale reverse data on
disk — the `callees` map is canonical, `callers` is derived.

## Internal consumers

| Consumer | Method | Output |
|---|---|---|
| `crabcc graph walk NAME --dir callers` | [`incoming(name, depth)`] | BFS over `callers` adjacency. |
| `crabcc graph walk NAME --dir callees` | [`outgoing(name, depth)`] | BFS over `callees` adjacency. |
| `crabcc graph cycles` | [`cycles()`] | Iterative Tarjan SCC; filters to size ≥ 2. |
| `crabcc graph orphans` | [`orphans()`] | Symbols defined in `callees` but absent from `callers` (no incoming edges). |
| `crabcc go` summary | `report.graph_edges = graph.edge_count` | Reports edge count in the post-init status block. |
| MCP `graph_*` tools | Same methods | Exposed as JSON-RPC tools by `crabcc-mcp`. |

The BFS walks intentionally don't dedup across depth levels — the walk
emits each `(name, depth)` pair as it's enqueued. Callers (`crabcc graph
walk`) deduplicate at the output stage if they care.

## Why a sidecar instead of always querying SQLite

Three reasons, in order of weight:

1. **Walk shape doesn't fit relational** — BFS over a graph is a
   pathological case for SQL: each level is a recursive CTE, and SQLite's
   cycle-detection is opt-in via `WITH RECURSIVE` syntax that bloats the
   query. Materialized BTreeMap walks are 5–10× faster on real graphs.
2. **Cycles are an SCC computation** — Tarjan needs random-access to the
   adjacency list during the DFS. An SQL implementation would scan
   `edges` per node visit; the in-memory map walks each edge exactly once.
3. **Reads are 100× more frequent than writes** — typical session: one
   `crabcc index`, dozens of `crabcc graph walk`. Paying ~5 ms once on
   build vs ~50 ms on every read amortizes immediately.

The trade-off: the sidecar is **derived data**. It can drift from the
SQLite truth source if a user runs `crabcc refresh` (which updates
`edges`) but doesn't rebuild the graph. `crabcc graph walk` mitigates
this with the `load_or_build_graph` shortcut on missing files; we don't
yet auto-rebuild on stale sidecars. That's a known gap — see the
research prompt at `docs/RESEARCH-graph-prompt.md`.

## File location, gitignore, lifecycle

- Path: `<repo>/.crabcc/graph.json`. Never written elsewhere.
- Should be in `.gitignore` (we add `.crabcc/` to .gitignore on init).
- `crabcc upgrade --apply` deletes it as part of the index-cleanup
  sweep so the next `crabcc index` rebuilds against the new binary's
  schema.
- `crabcc go` rebuilds it unconditionally on every invocation.

## Caveats

- The graph is **purely call-edge based**. We do not yet model field
  access, type-`extends`, trait/interface impls, or import-of-export
  edges. Adding any of those is a non-trivial extractor change in
  `crates/crabcc-core/src/extract.rs` plus a schema-additive column on
  `edges` — research prompt covers the design space.
- Symbol-name resolution is **lexical only**. Two functions named `Foo`
  in different modules collapse to one node. Resolving fully-qualified
  names would require carrying parent-symbol context through the
  extractor — also covered in the research prompt.

[`CallGraph`]: ../crates/crabcc-core/src/graph.rs
[`graph::CallGraph::build`]: ../crates/crabcc-core/src/graph.rs
[`build_from_edges`]: ../crates/crabcc-core/src/graph.rs
[`build_legacy`]: ../crates/crabcc-core/src/graph.rs
[`crabcc_cli::go::init`]: ../crates/crabcc-cli/src/go.rs
[`load_or_build_graph`]: ../crates/crabcc-cli/src/main.rs
[`incoming(name, depth)`]: ../crates/crabcc-core/src/graph.rs
[`outgoing(name, depth)`]: ../crates/crabcc-core/src/graph.rs
[`cycles()`]: ../crates/crabcc-core/src/graph.rs
[`orphans()`]: ../crates/crabcc-core/src/graph.rs
