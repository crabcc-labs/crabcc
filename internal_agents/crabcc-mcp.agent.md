# Internal Agent — crabcc-mcp specialist

You own `crates/crabcc-mcp/`. Read `internal_agents/shared.agent.md`
first. This file is the crate-specific context.

## What this crate does

`crabcc-mcp` is the **MCP server library**: stdio JSON-RPC 2.0 over
the crabcc query surface. The `crabcc-cli` binary's `--mcp` flag is
the entry point.

- **`serve_io<R: BufRead, W: Write>`** is the canonical loop (issue
  #89 slice 1 + the `read_until` / `from_slice` perf pass in PR #117).
- **Tool surface** — `sym`, `refs`, `callers`, `outline`, `files`,
  `fuzzy`, `prefix`, `grep`, plus the memory tools and graph ops.
- **OpenAPI spec** — `crates/crabcc-mcp/openapi.yaml` is generated
  from the tool descriptors. Don't hand-edit it; regenerate.
- **Slim vs dev surface** — issue #59. Default is the slim agent
  surface; `--dev` (or `CRABCC_MCP_DEV=1`) exposes `_openapi` +
  `_health` for diagnostics.

## Hot paths

- `serve_io` is on every MCP request. The PR #117 work (`read_until`
  + reused `Vec<u8>` + sonic-rs `from_slice` / `to_writer`) cut p50
  latency notably; preserve those choices unless you have benches
  showing a better baseline.
- Tool dispatch tables (`mcp::tools`) — large `match` on tool name.
  Don't refactor to a `HashMap` lookup without a perf gate (the
  match is monomorphised by LLVM into a jump table; HashMap is
  slower for ≤32 entries).

## Tests

- `tests/tool_coverage.rs` — every tool descriptor has a smoke test.
  When you add a new tool, add a row here AND add it to the OpenAPI
  spec.
- `benches/serve_io.rs` — micro-benches for the stdio loop.

## Cross-crate dependencies

- Consumes everything public from `crabcc-core` (the entire query
  side). Adding new public symbols to crabcc-core costs you nothing;
  changing existing signatures costs you a coordination window.
- Consumed only by `crabcc-cli`'s `--mcp` shim.
