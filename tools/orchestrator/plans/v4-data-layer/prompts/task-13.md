# Task 13 — MCP tool surface for the four new KG ops

## Context

Wave 3, parallel. Tasks 8–11 produced four KG-op modules under
`crates/crabcc-core/src/query/{blast_radius,why,hot_symbols,importers}.rs`,
and Task 12 wired them into the `crabcc graph` CLI surface. This task
exposes the same four ops as MCP tools so agent clients (Claude Code,
Cursor, etc.) can call them via JSON-RPC.

The existing MCP `graph` tools (`graph`, `graph_cycles`, `graph_orphans`)
live in `crates/crabcc-mcp/src/dispatch.rs` and are registered in the
`tools/list` schema in `crates/crabcc-mcp/src/schema.rs`. **This task is
scoped to `dispatch.rs` only** (schema.rs is not in the allow-list).

That looks asymmetric but it is intentional: the dispatch tier in this
codebase routes tools by `match tool` arm, and unknown tools return an
error from `dispatch_tool_inner` *before* `tools/list` is consulted. Adding
the four match arms here makes the tools callable end-to-end via
`tools/call`; an out-of-scope follow-up will surface them in `tools/list`
so they auto-discover. The CLI integration test (Task 15) and the
documentation tools both go through `tools/call` with an explicit tool
name, so this PR is functionally complete for those consumers.

## Pre-flight check

Before touching the file, verify that the upstream query modules exist
and exported the names this task wires up:

```bash
grep -q 'pub fn blast_radius' crates/crabcc-core/src/query/blast_radius.rs && \
grep -q 'pub fn why'          crates/crabcc-core/src/query/why.rs          && \
grep -q 'pub fn hot_symbols'  crates/crabcc-core/src/query/hot_symbols.rs  && \
grep -q 'pub fn importers'    crates/crabcc-core/src/query/importers.rs    || \
  { echo "task-13 FAIL: upstream KG modules missing expected pub fn signatures" >&2; exit 1; }

grep -q 'fn symbol_id_by_name_file' crates/crabcc-core/src/store.rs || \
  { echo "task-13 FAIL: Store::symbol_id_by_name_file (added in Task 2) is missing" >&2; exit 1; }
```

Run that as the first action. Non-zero exit → stop, do not touch any file.

## What to change

File: `crates/crabcc-mcp/src/dispatch.rs`

The current `match tool { ... }` block in `dispatch_tool_inner` has these
three graph arms (lines ~256–278):

```rust
        "graph" => {
            let name = arg_str(&args, "name")?.to_string();
            let dir = args
                .get("dir")
                .and_then(|v| v.as_str())
                .unwrap_or("callers");
            let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
            let g = load_or_build_graph(&store, root)?;
            let hits = if dir == "callees" {
                g.outgoing(&name, depth)
            } else {
                g.incoming(&name, depth)
            };
            Ok(serde_json::to_string(&hits)?)
        }
        "graph_cycles" => {
            let g = load_or_build_graph(&store, root)?;
            Ok(serde_json::to_string(&g.cycles())?)
        }
        "graph_orphans" => {
            let g = load_or_build_graph(&store, root)?;
            Ok(serde_json::to_string(&g.orphans())?)
        }
```

Two changes:

1. **Update the `"graph"` arm.** After Task 12 the `CallGraph::outgoing` /
   `CallGraph::incoming` methods take `i64` instead of `&str`. Resolve
   `name` to a `symbol_id` via `Store::find_by_name` + the new
   `Store::symbol_id_by_name_file` accessor (mirroring the
   `resolve_symbol_id` helper Task 12 added to the CLI).
2. **Add four new arms** for `graph.blast_radius`, `graph.why`,
   `graph.hot_symbols`, `graph.importers`. Tool names use dots (matching
   the `memory.*` namespacing convention already in this file at line
   ~137: `if tool.starts_with("memory.") { return memory::dispatch(...) }`).

### Edit site — replace the three graph arms

Find the exact block shown above (the three arms `"graph"`,
`"graph_cycles"`, `"graph_orphans"`) and replace it with:

```rust
        "graph" => {
            let name = arg_str(&args, "name")?.to_string();
            let dir = args
                .get("dir")
                .and_then(|v| v.as_str())
                .unwrap_or("callers");
            let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
            let symbol_id = resolve_symbol_id(&store, &name)?;
            let g = load_or_build_graph(&store, root)?;
            let hits = if dir == "callees" {
                g.outgoing(symbol_id, depth)
            } else {
                g.incoming(symbol_id, depth)
            };
            Ok(serde_json::to_string(&hits)?)
        }
        "graph_cycles" => {
            let g = load_or_build_graph(&store, root)?;
            Ok(serde_json::to_string(&g.cycles())?)
        }
        "graph_orphans" => {
            let g = load_or_build_graph(&store, root)?;
            Ok(serde_json::to_string(&g.orphans())?)
        }
        "graph.blast_radius" => {
            let symbol = arg_str(&args, "symbol")?.to_string();
            let depth = args.get("depth").and_then(|v| v.as_u64()).map(|n| n as usize);
            let kind = args.get("kind").and_then(|v| v.as_str()).map(String::from);
            let symbol_id = resolve_symbol_id(&store, &symbol)?;
            let hits = crabcc_core::query::blast_radius::blast_radius(
                &store,
                symbol_id,
                depth,
                kind.as_deref(),
            )?;
            Ok(serde_json::to_string(&hits)?)
        }
        "graph.why" => {
            let src = arg_str(&args, "src")?.to_string();
            let dst = arg_str(&args, "dst")?.to_string();
            let max_depth = args
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let src_id = resolve_symbol_id(&store, &src)?;
            let dst_id = resolve_symbol_id(&store, &dst)?;
            let path = crabcc_core::query::why::why(&store, src_id, dst_id, max_depth)?;
            Ok(serde_json::to_string(&path)?)
        }
        "graph.hot_symbols" => {
            let top = args.get("top").and_then(|v| v.as_u64()).map(|n| n as usize);
            let kind = args.get("kind").and_then(|v| v.as_str()).map(String::from);
            let hits = crabcc_core::query::hot_symbols::hot_symbols(
                &store,
                top,
                kind.as_deref(),
            )?;
            Ok(serde_json::to_string(&hits)?)
        }
        "graph.importers" => {
            let path = arg_str(&args, "path")?.to_string();
            let depth = args.get("depth").and_then(|v| v.as_u64()).map(|n| n as usize);
            let hits = crabcc_core::query::importers::importers(&store, &path, depth)?;
            Ok(serde_json::to_string(&hits)?)
        }
```

### Add the `resolve_symbol_id` helper

The four new arms (and the updated `"graph"` arm) all need to resolve a
user-typed name to a `symbol_id`. Add this private helper function at the
bottom of `dispatch.rs`, just before the closing brace of the file. If
there is no closing brace (the file is module-flat) append the function as
the last item. The exact body:

```rust

/// Resolve a user-typed symbol name to a `symbols.id`. Mirrors the CLI
/// helper of the same name in `crabcc-cli::main`. Uses
/// `Store::find_by_name` (which already powers `lookup sym`); on multiple
/// hits, returns the first one — MCP callers can disambiguate by passing
/// a qualified name.
fn resolve_symbol_id(store: &Store, name: &str) -> Result<i64> {
    let hits = store.find_by_name(name)?;
    if hits.is_empty() {
        return Err(anyhow::anyhow!("symbol not found: {name}"));
    }
    let first = &hits[0];
    store.symbol_id_by_name_file(name, &first.file, first.line_start)
}
```

The `Store` type is already in scope at the top of the file
(`use crabcc_core::{fts::Fts, index, outline, query, store::Store};` at
line 15). No new `use` directives are required for `resolve_symbol_id`.

### JSON-schema note

You do not edit `schema.rs` in this task. The four new tools are reachable
via `tools/call` immediately because dispatch routes by name match, not by
schema membership. A follow-up PR (out-of-scope here) will publish the
inputSchemas in `tools_def_for(dev)` so the tools auto-discover.

For your own reference, the inputSchemas a follow-up will register are:

| Tool | Required args | Optional args |
|---|---|---|
| `graph.blast_radius` | `symbol: string` (symbol name) | `depth: integer` (cap, default uncapped), `kind: string` (`'call' \| 'ref' \| 'all'`, default all) |
| `graph.why` | `src: string`, `dst: string` (symbol names) | `max_depth: integer` (cap, default uncapped) |
| `graph.hot_symbols` | — | `top: integer` (limit, default 20), `kind: string` (`'call' \| 'ref' \| 'all'`, default all) |
| `graph.importers` | `path: string` (file-or-module path) | `depth: integer` (cap, default 1) |

## Definition of done

- The three existing graph arms in `dispatch_tool_inner` are updated (the
  `"graph"` arm gains the `resolve_symbol_id` call; the other two are
  byte-identical to today).
- Four new match arms (`graph.blast_radius`, `graph.why`,
  `graph.hot_symbols`, `graph.importers`) exist after the `"graph_orphans"`
  arm.
- A new private `resolve_symbol_id(store: &Store, name: &str) ->
  Result<i64>` helper sits at the bottom of the file.
- No other file in the workspace is touched.

Do not run `cargo build`, `cargo test`, or any other build or test command.

Do not modify any other file. Do not invent extra files.

Then commit with this exact message:

    feat(mcp): expose blast-radius/why/hot-symbols/importers as MCP tools
