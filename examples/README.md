# crabcc examples

One file per topic. Pick the one that matches your question.

## CLI

| Topic | File | When to read |
|---|---|---|
| Indexing & refresh | [`indexing.md`](./indexing.md) | First-time setup, incremental updates. |
| Find a symbol | [`sym.md`](./sym.md) | "Where is `Foo` defined?" |
| References | [`refs.md`](./refs.md) | "All references to `UserId`." |
| Callers | [`callers.md`](./callers.md) | "What calls `handleAuth`?" |
| File outline | [`outline.md`](./outline.md) | "What's in this file?" — replaces `Read`. |
| List files | [`files.md`](./files.md) | Replaces `ls -R` / `find -name`. |
| Fuzzy + prefix | [`fuzzy-prefix.md`](./fuzzy-prefix.md) | Misremembered names, partial names. |
| `jq` pipelines | [`jq-pipelines.md`](./jq-pipelines.md) | Project / filter / group crabcc JSON. |
| Token-savings tracker | [`track.md`](./track.md) | "How much have I saved?" |
| Cheatsheet (all-in-one) | [`CLI.md`](./CLI.md) | Quick scan of everything. |

## MCP

| Topic | File | When to read |
|---|---|---|
| MCP server setup | [`mcp-setup.md`](./mcp-setup.md) | Wiring `crabcc --mcp` into Claude Code. |
| Wire protocol | [`wire-protocol.md`](./wire-protocol.md) | Wire-level JSON-RPC walkthrough. |
| Full MCP cheatsheet | [`MCP.md`](./MCP.md) | All tools, all examples. |

## Conventions

- All commands assume you've run `crabcc index` once in the repo.
- Output shown is JSON (compact). Pipe through `jq` for human reading.
- "raw" comparisons are the equivalent `rg`/`grep`/`find` invocation an agent
  would otherwise run.
