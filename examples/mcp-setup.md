# `crabcc --mcp` — MCP server setup

> Run crabcc as a JSON-RPC 2.0 stdio server for direct LLM tool calls.

## Wiring into Claude Code

### User-level (everywhere)

Add to `~/.claude.json` under `mcpServers`:

```json
{
  "mcpServers": {
    "crabcc": {
      "command": "crabcc",
      "args": ["--mcp"]
    }
  }
}
```

Then `/reload-plugins` or restart Claude Code. Claude Code prompts to trust the
new server on first use; approve once.

### Project-level

Drop a `.mcp.json` at the repo root:

```json
{
  "mcpServers": {
    "crabcc": {
      "command": "crabcc",
      "args": ["--mcp"]
    }
  }
}
```

`--root` defaults to cwd. For multi-repo setups, spawn one server per repo with
`args: ["--mcp", "--root", "/abs/path"]`.

## Tools exposed

| Tool       | Purpose                                                                  |
|------------|--------------------------------------------------------------------------|
| `sym`      | Find a symbol by exact name.                                             |
| `refs`     | All identifier references. Modes: `hits`, `files`, `count`.              |
| `callers`  | All call sites (bare + method-receiver). Modes: `hits`, `files`, `count`.|
| `outline`  | Every symbol in a file, ordered by line.                                 |
| `files`    | List indexed files. Filters: `under`, `lang`, `ext`, `limit`.            |
| `index`    | Full rebuild.                                                            |
| `refresh`  | Incremental refresh (mtime + sha256).                                    |
| `fuzzy`    | Levenshtein distance 2 over symbol names (token-aware, native).          |
| `prefix`   | Case-insensitive starts-with over symbol names (token-aware, native).    |

For wire-level request/response examples, see [`wire-protocol.md`](./wire-protocol.md).
For the all-in-one cheatsheet, see [`MCP.md`](./MCP.md).

## CLI vs MCP — when to use which

| Concern                    | CLI subprocess               | MCP                                |
|----------------------------|------------------------------|------------------------------------|
| Startup overhead           | ~5–15 ms per call            | one-time process spawn             |
| Stdout parsing             | agent parses crabcc JSON     | MCP harness parses for you         |
| Concurrent calls           | spawns more processes        | persistent session                 |
| Tool discoverability       | via skill / docs             | via `tools/list`                   |
| Permission model           | per-Bash-invocation prompt   | one MCP-server approval            |
| Best for                   | one-off shell scripting      | agent loops with many lookups      |

For agents that make many lookups in one session, MCP wins on both latency and
approval-prompt count.

## Verifying the server works

```bash
{
  printf '{"jsonrpc":"2.0","id":1,"method":"initialize"}\n'
  printf '{"jsonrpc":"2.0","id":2,"method":"tools/list"}\n'
} | crabcc --mcp
```

Expected: two JSON lines back, with `serverInfo.name == "crabcc-mcp"` and an array
of 9 tools.
