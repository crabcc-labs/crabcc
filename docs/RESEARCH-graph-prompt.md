# Research prompt — call-graph evolution for crabcc

> Drop-in prompt for further research into where the crabcc call-graph
> sidecar should go next, particularly through a Rust-ecosystem lens.
> Feed this verbatim to a research-capable LLM (Claude with web search,
> Perplexity, etc.) — the sections that follow are written so the model
> can split work across them in parallel.

---

## Context (what the model needs before answering)

`crabcc` is a Rust CLI + MCP server that indexes a repository's symbols
into a SQLite database (`.crabcc/index.db`) and surfaces them via four
agent-friendly primitives: `sym`, `refs`, `callers`, `outline`. Beyond
that, it builds a derived **call-graph sidecar** at
`.crabcc/graph.json` from the populated `edges` table. The sidecar is
the canonical source for `crabcc graph walk / cycles / orphans`.

The current implementation is described in `docs/GRAPH.md`. Key points:

- **Storage**: pretty-printed JSON, `BTreeMap<String, BTreeSet<String>>`
  for both forward (`callees`) and reverse (`callers`) adjacency, plus
  a precomputed `edge_count`.
- **Build path**: a single `SELECT … FROM edges` scan + BTreeMap
  inserts. Legacy fallback walks symbols × files with ast-grep.
- **Reads**: BFS for `walk`, iterative Tarjan SCC for `cycles`, set
  arithmetic for `orphans`.
- **Resolution**: lexical only — symbol names collapse across modules.
- **Edge taxonomy**: today it's `kind = 'call'` only. No field-access,
  type-extension, or import-of-export edges.
- **Refresh model**: the sidecar is derived data; it can drift from the
  `edges` table if the user runs `crabcc refresh` without `graph build`.

The dependencies in play (workspace `Cargo.toml`):

- `rusqlite` 0.31 (`bundled` feature) for the index store
- `serde` / `serde_json` for the sidecar I/O
- `tree-sitter` 0.22 for parsing
- `ast-grep-core` 0.30 for pattern-based match passes
- `tantivy` 0.22 for fuzzy/prefix sidecar (fts.rs)
- `fsst-rs` 0.5 for signature-column compression
- `notify-debouncer-mini` 0.4 for the `crabcc watch` hook

We deliberately avoid heavy graph libraries (no `petgraph`, no
`indradb`, no `oxigraph`) so far. That decision is part of what we
want challenged.

## Open questions to research

Treat each of the headings below as an independent research target.
Cite sources (crates.io, GitHub, papers, docs.rs) for any concrete
recommendation.

### 1. Storage layout — JSON vs binary vs sqlite-native

JSON is diff-friendly and trivial to parse but pays a non-trivial
serialize cost on large repos (~5–10% of total `graph build` time on
13k-file fixtures). Investigate:

- `bincode` vs `rmp-serde` (msgpack) vs `postcard` for the same
  `CallGraph` struct. Compare encoded size + ser/deser time on a
  realistic 50k-edge graph.
- Storing the graph **inside** `.crabcc/index.db` as a dedicated table
  — write the adjacency rows on every index pass, no separate sidecar.
  Trade-offs: index file size, BFS query latency vs in-memory walk,
  staleness vs the current "build is a separate step" model.
- Persistent KV stores: `redb` (pure Rust, SQLite-style ACID, mmap'd),
  `sled` (lock-free LSM but in maintenance mode), `fjall`. Which gives
  the best read latency for a graph with ~50k–500k edges and frequent
  partial-graph mutations (incremental refresh)?
- Memory-mapped formats — flatbuffers/cap'n proto. Win condition: load
  is `mmap` + `Box::leak`, no parse step at all. Loss condition: writes
  are far more painful than JSON.

### 2. The `petgraph` question

We avoided `petgraph` to keep the dep tree small. The cost: we hand-rolled
`incoming`, `outgoing`, `cycles` (Tarjan), `orphans`. The benefit: zero
extra deps. Is that still the right call at v2.3?

- What's `petgraph`'s maintained subset of algorithms in 2026? Compare
  to what we need (BFS, SCC, topological sort, dominator tree —
  potentially needed for orphan ranking, currently a flat set).
- Memory overhead per node — `petgraph::Graph<String, ()>` interns
  strings via an external map. Is that a regression for our workload?
- Is `petgraph::stable_graph::StableGraph` worth it for incremental
  edge add/remove? We want `O(1)` removal of an edge when a file is
  re-indexed.
- Alternatives: `petgraph` vs `graph` (the crate) vs `graphviz-rust`
  vs hand-rolled. Score on (a) algorithmic completeness, (b) maint
  cadence, (c) compile-time impact, (d) `no_std` story.

### 3. Incremental graph maintenance

`crabcc refresh` already updates the `edges` table by mtime. The graph
sidecar is rebuilt unconditionally on `graph build`. Goal: make the
sidecar incremental.

- Algorithm question: given the set of files re-extracted by
  `refresh`, which subgraph needs invalidation? Can we compute a
  "blast radius" (all symbols whose `callers` or `callees` set changed)
  without a full rebuild?
- Persistence question: should the sidecar carry a per-file-id watermark
  that the indexer bumps so a stale sidecar can be detected by epoch
  comparison?
- Data-structure question: would a CRDT-style adjacency-list (per-file
  contributions, merged on read) eliminate the rebuild step at the cost
  of read complexity? Cite real-world precedent (build-graph systems,
  bazel's action graph, CodeQL's dataflow graph).

### 4. Edge taxonomy — beyond calls

Today's edges are `(src_symbol, dst_name, kind='call', src_file_id,
src_line)`. We want to extend to:

- **Type-extension** edges: `class Foo extends Bar` → `Foo --extends--> Bar`.
- **Trait/interface impl** edges: `impl Trait for Foo` →
  `Foo --impl--> Trait`.
- **Field/method access** edges: `obj.method()` already exists as a
  call; `obj.field` does not. Is that worth tracking?
- **Import edges**: `use foo::bar` → cross-file dependency. Could power
  a `crabcc graph deps` command.

For each:
- What tree-sitter node-kinds need extracting per language (TS/TSX,
  JS, Rust, Go, Python, Ruby)?
- What's the schema-additive column shape on `edges` (an `enum kind`
  vs separate tables vs sparse columns)?
- How does the sidecar change — single `CallGraph`, or one map per
  edge kind? What's the BFS contract when the user asks for "what
  depends on X" without specifying kind?

### 5. Cycle detection at scale

Tarjan SCC is O(V + E). On a 13k-file fixture (~50k edges) we run in
single-digit milliseconds. At what scale does this break?

- Real-world graph sizes: how big do call-graphs get on monorepos?
  Look at the LLVM build graph (~1M edges?), the Linux kernel,
  Chromium, mc-mothership-class Rails apps. What's the regime where
  Tarjan-on-load stops being viable and we need streaming or
  partition-based cycle detection?
- Alternative algorithms: Johnson's "find all elementary circuits" vs
  Tarjan SCC vs Pearce's SCC. We currently filter SCCs to size ≥ 2 to
  surface mutual recursion only — does that constraint open up a
  cheaper algorithm specialized for `recursion-only`?
- Streaming SCC: any Rust crates? Pregel-style frameworks (`rayon` +
  shared atomic state) for embarrassingly-parallel graph-walk?

### 6. Querying the graph — SQL recursive CTEs reconsidered

We rejected SQLite recursive CTEs in favour of the sidecar. Re-test
that decision in 2026:

- SQLite 3.46+ added `MATERIALIZED` / `NOT MATERIALIZED` hints. Do they
  change the recursive-CTE perf story for a `WITH RECURSIVE walk(name,
  depth) AS (…)` over our `edges` table?
- What's `sqlite-vec`'s graph story (the `sqlite-vec` extension we
  already use for ANN)? Are there other SQLite extensions that give
  us BFS / SCC primitives without leaving SQL? `sqlite-graph`,
  `closure_table`?
- Is there a hybrid where the sidecar holds adjacency but SQLite holds
  attribution metadata (which file, which line) and we join on read?

### 7. Visualization + export

Adjacent topic — we don't visualize today. What's the lowest-cost path
to a `crabcc graph render --format dot|svg|mermaid`?

- DOT generation in pure Rust — `petgraph::dot::Dot`, `graphviz-rust`,
  hand-rolled string concat?
- Mermaid output for embedding into PR descriptions (markdown-native)?
- Web rendering — could we ship a static HTML viewer that loads
  `graph.json` directly via `fetch()`? Compare to mdBook's existing
  `mermaid` plugin.

## Output format we want back from the model

For each numbered section, produce:

1. **Recommendation** — one paragraph, opinionated, with a confidence
   level (`high` / `medium` / `low`). No fence-sitting.
2. **Trade-offs** — bullet list of what we lose by following the
   recommendation.
3. **Alternative** — the next-best option, with the threshold ("when
   would we pick this instead?").
4. **Sources** — links to crates.io, docs.rs, GitHub repos, blog
   posts, papers. At least three per section. Newest first.
5. **Migration sketch** — if the recommendation is non-trivial, a
   3–7 step bulleted plan. No code blocks unless absolutely needed.

After the per-section answers, append a final "**Cross-cutting
themes**" section that calls out anything that was true across multiple
research areas (e.g., "all four storage candidates point at switching
the on-disk format before optimizing the algorithms").

Stay in this repo's idiom: terse, concrete, no marketing language. We
don't want a survey of every option; we want the recommendations the
model would defend in a code review.
