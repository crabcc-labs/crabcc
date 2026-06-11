---
name: crabcc-mcp
description: Use the crabcc MCP server for symbol lookups, code navigation, memory operations, file reads, and Mastodon social posting. The MCP server exposes 30+ tools over stdio or HTTP (JSON/MessagePack/SSE). Use this when you have an MCP client connected to a crabcc-mcp server.
---

# crabcc MCP server — full agent guide

> **Source files:** [`crates/crabcc-mcp/src/`](../../crates/crabcc-mcp/src/)
> **Tool catalog:** [`skill/crabcc-mcp/.tools`](.tools)
> **Dispatch:** [`dispatch.rs`](../../crates/crabcc-mcp/src/dispatch.rs)
> **Transport:** [`transport.rs`](../../crates/crabcc-mcp/src/transport.rs)
> **Mastodon:** [`mastodon.rs`](../../crates/crabcc-mcp/src/mastodon.rs)
> **Dashboard:** [`dashboard.html`](../../crates/crabcc-mcp/src/dashboard.html)
> **CLI entry:** [`crabcc-cli/src/main.rs`](../../crates/crabcc-cli/src/main.rs)
> **Core lib:** [`crabcc-core/src/lib.rs`](../../crates/crabcc-core/src/lib.rs)
> **Memory lib:** [`crabcc-memory/src/lib.rs`](../../crates/crabcc-memory/src/lib.rs)
> **AGENTS.md:** [`AGENTS.md`](../../AGENTS.md)

The crabcc MCP server gives you **symbol-aware code queries**, **persistent memory**, **structured file reads**, **graph analysis**, and **Mastodon social posting** — all through a single MCP connection. It replaces `grep`, `find`, `ls`, raw file reads, and ad-hoc note-taking for code-related work.

## How to connect

**stdio** (default, most clients):
```json
{ "command": "crabcc", "args": ["--mcp"] }
```
> Source: [`crabcc-cli/src/main.rs`](../../crates/crabcc-cli/src/main.rs) — `Cmd::Mcp` arm

**HTTP** (for remote/SSE clients):
```bash
crabcc serve --addr 127.0.0.1:8765
# Then connect your MCP client to http://127.0.0.1:8765/mcp
```
> Source: [`transport.rs`](../../crates/crabcc-mcp/src/transport.rs) — `serve_http()`

**HTTP + SSE** (streaming responses):
```bash
crabcc serve --addr 127.0.0.1:8765
# Client sends Accept: text/event-stream on POST /mcp
```
> Source: [`transport.rs`](../../crates/crabcc-mcp/src/transport.rs) — `accepts_sse()`, `sse_event_with_id()`

**HTTP + MessagePack** (30-50% smaller, faster):
```bash
# Client sends Content-Type: application/msgpack + Accept: application/msgpack
```
> Source: [`transport.rs`](../../crates/crabcc-mcp/src/transport.rs) — `Format::MessagePack`, `rmp_serde`

---

## Tool categories

### Symbol index (code lookups)

These are the fastest way to answer code questions. Prefer them over `grep`, `find`, `ls`, or reading whole files.

> Source: [`dispatch.rs`](../../crates/crabcc-mcp/src/dispatch.rs) — `dispatch_tool_inner()`
> Schema: [`schema.rs`](../../crates/crabcc-mcp/src/schema.rs) — `tools_def_symbol()`
> Core: [`crabcc-core/src/query/`](../../crates/crabcc-core/src/query/) — query engine

| Tool | Use when | Example |
|---|---|---|
| `sym` | "Where is X defined?" | `sym name="handleAuth"` |
| `refs` | "What references X?" | `refs name="UserId" mode="files" limit=20` |
| `callers` | "Who calls X?" | `callers name="handleAuth" mode="count"` |
| `outline` | "What's in this file?" | `outline file="src/main.rs"` |
| `fuzzy` | "Misspelled/approximate name" | `fuzzy query="Asseessment"` |
| `prefix` | "Names starting with…" | `prefix query="getUser"` |
| `files` | "List code files" | `files under="app/models" ext="rb"` |
| `affected` | "Which tests cover my edit?" | `affected since="HEAD~3"` |
| `test_context` | "Give me context to write a test for X" | `test_context name="handleAuth"` |

**Token-shaping flags** (use these aggressively to minimize context usage):

| Flag | Effect | When |
|---|---|---|
| `mode="count"` | `{"count": N}` only (~3 tokens) | "How many?" questions |
| `mode="files"` | Deduped file list (~88% smaller) | "Which files?" questions |
| `mode="summary"` | `{by_file: {path: N, ...}}` (~95% smaller) | "Distribution?" questions |
| `limit=N` | Cap result size | Always set a reasonable limit |
| `stream=true` | NDJSON (one hit per line) | Piping to jq or bulk processing |
| `since="REF"` | Restrict to files changed since git ref | After edits, PR reviews |

**Idempotency / cache revalidation:**
- `if_changed="<fingerprint>"` — on match, returns `{unchanged: true}`. Saves re-parsing unchanged results.

### Memory (persistent agent notes)

The memory layer stores and retrieves agent notes, code context, and session history. Backed by SQLite with FTS5 + vector search.

> Source: [`memory.rs`](../../crates/crabcc-mcp/src/memory.rs)
> Core: [`crabcc-memory/src/palace.rs`](../../crates/crabcc-memory/src/palace.rs)
> Schema: [`crates/crabcc-memory/schema/001_init.sql`](../../crates/crabcc-memory/schema/001_init.sql)

| Tool | When |
|---|---|
| `memory.init` | First use — idempotent, creates `.crabcc/memory.db` |
| `memory.remember` | Store a note, code snippet, or reflection |
| `memory.search` | Find relevant memories (hybrid: BM25 + vector) |
| `memory.get` | Fetch one memory by id |
| `memory.list` | Browse recent memories |
| `memory.delete` | Remove by id/source/all |
| `memory.forget` | Delete + VACUUM (reclaim disk) |
| `memory.backup` | Snapshot memory.db (safe on live DB) |
| `memory.count` | How many memories stored |
| `memory.health` | Ok / Degraded / Down |
| `memory.mine_project` | Bulk-import all project files as memories |
| `memory.mine_sessions` | Bulk-import Claude Code transcripts |
| `memory.remind_set` | Schedule a reminder (send_later) |
| `memory.remind_poll` | Fetch due reminders |

**Best practices:**
- Use `memory.search` with `mode="hybrid"` (default) for best recall
- Tag memories with `wing` and `room` for namespacing
- Use `memory.remember` after important findings — the next agent will find them
- `memory.remind_set` + `memory.remind_poll` replaces Claude Code's `send_later`

### Structured file read

> Source: [`dispatch.rs`](../../crates/crabcc-mcp/src/dispatch.rs) — `"read"` arm
> Core: [`crabcc-memory/src/read.rs`](../../crates/crabcc-memory/src/read.rs)

| Tool | When |
|---|---|
| `read` | Read a file, with mode-aware caching |

**Modes:**
- `mode="auto"` — full content on first read, outline stub on re-read (30× cheaper)
- `mode="full"` — always full content
- `mode="stub"` — outline only (function signatures, no bodies)
- `mode="entropy"` — filter lines below entropy threshold (good for logs)

**Session-aware:** Pass `session_id` to cache per-conversation. Skipping it reads `$CRABCC_SESSION_ID`.

### Code editing + validation

> Source: [`dispatch.rs`](../../crates/crabcc-mcp/src/dispatch.rs) — `"write_file"` arm
> Core: [`crabcc-core/src/validate.rs`](../../crates/crabcc-core/src/validate.rs)

| Tool | When |
|---|---|
| `write_file` | Write a file and get parse validation + symbol diff + broken caller detection |

`write_file` returns: `parse_ok`, `symbol_diff` (added/removed/changed), and `broken_caller_files` — faster than a compiler round-trip.

### Graph analysis

> Source: [`dispatch.rs`](../../crates/crabcc-mcp/src/dispatch.rs) — `"graph"` / `"graph.*"` arms
> Core: [`crabcc-core/src/graph.rs`](../../crates/crabcc-core/src/graph.rs)

| Tool | When |
|---|---|
| `graph` | BFS walk of callers/callees from a symbol |
| `graph.blast_radius` | Transitive dependency explosion |
| `graph.why` | Find the path between two symbols |
| `graph.hot_symbols` | Most-called symbols |
| `graph.importers` | Files that import a given file |
| `graph_cycles` | Mutual recursion / SCC detection |
| `graph_orphans` | Symbols with no incoming callers |

### Index lifecycle

> Source: [`dispatch.rs`](../../crates/crabcc-mcp/src/dispatch.rs) — `"index"` / `"refresh"` / `"upgrade"` arms
> Core: [`crabcc-core/src/index.rs`](../../crates/crabcc-core/src/index.rs), [`crabcc-core/src/upgrade.rs`](../../crates/crabcc-core/src/upgrade.rs)

| Tool | When |
|---|---|
| `index` | Build a fresh index (wipes existing) |
| `refresh` | Incremental update (mtime+sha256 diff) |
| `upgrade` | Check for newer crabcc release |

### Mastodon social (v6.3)

> Source: [`mastodon.rs`](../../crates/crabcc-mcp/src/mastodon.rs)
> Config: [`deploy/bots/post.mjs`](../../../crabcc.app-social/deploy/bots/post.mjs) (existing Node.js poster)
> Bots: [`deploy/bots/bots.config.json`](../../../crabcc.app-social/deploy/bots/bots.config.json)
> Docs: [`deploy/bots/POSTING.md`](../../../crabcc.app-social/POSTING.md)

| Tool | When |
|---|---|
| `mastodon.post` | Write a status (reflection, release note, summary) |
| `mastodon.read` | Read recent posts from a timeline |
| `mastodon.verify` | Check token validity and instance reachability |

**Auth:** Set `MASTODON_TOKEN` in the environment (or per-bot `<BOT>_TOKEN`). Create tokens at `<instance>/settings/applications` with scope `write:statuses`.

**Rate limit:** 5 attempts per token per 48 hours. Every response includes `rate_limit` metadata with `attempts_used`, `attempts_left`, and `resets_in_seconds`.

**Idempotency:** Pass `idempotency_key` (release id, commit sha) to `mastodon.post` — Mastodon deduplicates for ~1h. Without one, a hash is auto-generated.

**Security:** base_url enforced `https://`, hashtag stripped to `[a-zA-Z0-9_]`, idempotency_key filtered to `[a-zA-Z0-9._:-]` capped 128 bytes, token env-only, body size capped 16 MiB. 27 security tests cover SSRF, injection, env isolation, null safety.

### Meta tools (dev mode only)

> Source: [`dispatch.rs`](../../crates/crabcc-mcp/src/dispatch.rs) — `dispatch_meta()`

Available when `CRABCC_MCP_DEV=1` or `crabcc --mcp --dev`:

| Tool | When |
|---|---|
| `_openapi` | Dump the OpenAPI 3.1 spec for this server |
| `_health` | Liveness probe (server name, version, tool count) |

---

## Transport: picking the right format

> Source: [`transport.rs`](../../crates/crabcc-mcp/src/transport.rs) — `Format` enum, `serialize_body()`, `deserialize_body()`

| Format | Header | Best for |
|---|---|---|
| JSON (default) | `Content-Type: application/json` | Human-readable, debugging |
| MessagePack | `Content-Type: application/msgpack` | 30-50% smaller, faster deserialize |
| SSE | `Accept: text/event-stream` | Streaming responses, server push |
| gzip | `Accept-Encoding: gzip` | Compression on large payloads |

MessagePack is the fastest end-to-end. Combine with gzip for maximum throughput.

---

## Dashboard

> Source: [`dashboard.html`](../../crates/crabcc-mcp/src/dashboard.html) (216 lines, embedded at compile time)
> Stats endpoint: [`transport.rs`](../../crates/crabcc-mcp/src/transport.rs) — `GET /stats` → `mastodon::gather_stats()`

Point a browser at `http://127.0.0.1:<port>/` for a live admin dashboard showing:
- Uptime
- Rate limit status (per-token progress bars)
- Recent request history (tool, latency, payload size)
- Cache statistics

No auth needed on loopback. With `--token`, all endpoints require `Authorization: Bearer <token>`.

---

## Golden rules

1. **Reach for `sym`/`refs`/`callers` before `grep` or reading files.** They're 47–4400× faster and 85% smaller.
2. **Use `mode` flags aggressively.** "How many?" → `mode="count"`. "Which files?" → `mode="files"`.
3. **Set `limit` on every query.** Unlimited results waste context.
4. **Use `memory.remember` after findings.** The next agent (or you tomorrow) will thank you.
5. **Use `mastodon.post` for release notes, not for chat.** The bots have voices — match them.
6. **Never paste tokens into MCP args.** Use environment variables. The Mastodon tools enforce this.
7. **Prefer MessagePack over JSON for bulk data.** Same content, 30-50% fewer bytes.
8. **Check the dashboard before debugging.** It shows rate limits, recent errors, and cache state.
