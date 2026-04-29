// MCP server (stdio) — exposes the same verbs as the CLI as MCP tools:
//   sym, refs, callers, outline, grep, index, refresh
//
// v1 plan: hand-rolled JSON-RPC over stdio, matching the MCP spec.
// If we want SDK ergonomics later, swap to `rmcp` (Rust MCP SDK).

use anyhow::Result;
use std::path::Path;

pub async fn serve_stdio(_root: &Path) -> Result<()> {
    // TODO: read JSON-RPC frames from stdin, dispatch to crabcc-core,
    //       write responses to stdout. Implement: initialize, tools/list,
    //       tools/call.
    eprintln!("crabcc-mcp: stdio server not yet implemented");
    Ok(())
}
