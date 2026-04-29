# crabcc MCP server — examples

> Same symbol-aware lookups, exposed as an MCP stdio server for direct LLM tool calls.
> No subprocess hop, no `claude -p`, no parsing JSON-from-stdout. Just `tools/call`.

The MCP server is the same binary as the CLI, started with `--mcp`:

```bash
crabcc --mcp --root /path/to/repo
```

It speaks JSON-RPC 2.0 over stdio per the MCP spec. Below: every wire-level interaction you need.

---

## 1. Wiring it into Claude Code

Drop this in `.mcp.json` at the repo root (or merge into your global `~/.claude/settings.json`):

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

`--root` defaults to cwd. For multi-repo setups, spawn one server per repo with `args: ["--mcp", "--root", "/abs/path"]`.

---

## 2. Initialize handshake

Every MCP session starts with `initialize`. crabcc replies with its protocol version and capabilities:

**Request**

```json
{"jsonrpc":"2.0","id":1,"method":"initialize"}
```

**Response**

```json
{"jsonrpc":"2.0","id":1,"result":{
  "protocolVersion":"2024-11-05",
  "capabilities":{"tools":{}},
  "serverInfo":{"name":"crabcc-mcp","version":"0.1.0"}
}}
```

---

## 3. List available tools — `tools/list`

```json
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
```

Returns an array of 9 tools: `sym`, `refs`, `callers`, `outline`, `files`, `index`, `refresh`, `fuzzy`, `prefix`. Each has a JSON-Schema input definition.

Excerpt — the `refs` tool:

```json
{
  "name":"refs",
  "description":"Find every identifier reference to `name` across the indexed repo. Use mode='files' for 'which files reference X', mode='count' for 'how many'.",
  "inputSchema":{
    "type":"object",
    "properties":{
      "name":{"type":"string","description":"symbol name"},
      "mode":{"type":"string","enum":["hits","files","count"],
              "description":"Output shape. 'hits' = full {file,line,col,snippet} list (default). 'files' = deduped file paths only (~70% smaller). 'count' = `{count: N}` only (smallest)."},
      "limit":{"type":"integer","description":"Cap result size. Omit for unlimited."}
    },
    "required":["name"]
  }
}
```

---

## 4. Find a symbol — `tools/call sym`

```json
{
  "jsonrpc":"2.0","id":3,
  "method":"tools/call",
  "params":{
    "name":"sym",
    "arguments":{"name":"Assessment"}
  }
}
```

**Response** — text content block whose body is the same compact JSON the CLI emits:

```json
{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text",
"text":"[{\"name\":\"Assessment\",\"kind\":\"class\",\"file\":\"app/models/assessment.rb\",\"line_start\":1,…}]"
}]}}
```

---

## 5. Token-shaping calls — `refs` and `callers`

The same three modes available on the CLI (`--files-only`, `--count`, `--limit`) map to MCP `arguments`:

### Just count

```json
{"jsonrpc":"2.0","id":4,"method":"tools/call",
 "params":{"name":"callers","arguments":{"name":"find_by","mode":"count"}}}
```

→ `{"count":475}` — **3 tokens** instead of 16k for the full hit list.

### Just file paths

```json
{"jsonrpc":"2.0","id":5,"method":"tools/call",
 "params":{"name":"refs","arguments":{"name":"Assessment","mode":"files","limit":10}}}
```

→ `{"files":["app/builders/...","app/models/..."]}`

### Cap full hit list

```json
{"jsonrpc":"2.0","id":6,"method":"tools/call",
 "params":{"name":"callers","arguments":{"name":"find_by","limit":5}}}
```

---

## 6. List indexed files — `tools/call files`

```json
{"jsonrpc":"2.0","id":7,"method":"tools/call",
 "params":{"name":"files","arguments":{"under":"app/models","ext":"rb","limit":5}}}
```

→ `["app/models/ab_relationship.rb", "app/models/abid.rb", …]`

Replaces an `ls -R` or `find -name` round-trip.

---

## 7. File outline — `tools/call outline`

```json
{"jsonrpc":"2.0","id":8,"method":"tools/call",
 "params":{"name":"outline","arguments":{"file":"app/models/assessment.rb"}}}
```

→ Every symbol in the file ordered by line. Use `line_start`/`line_end` to selectively `Read` only the methods you need.

---

## 8. Fuzzy / prefix search

```json
{"jsonrpc":"2.0","id":9,"method":"tools/call",
 "params":{"name":"fuzzy","arguments":{"query":"Authentcator"}}}
```

```json
{"jsonrpc":"2.0","id":10,"method":"tools/call",
 "params":{"name":"prefix","arguments":{"query":"getUser"}}}
```

Both return `[{name,kind,file,line,parent,score}, …]`.

---

## 9. Index management

```json
{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"refresh","arguments":{}}}
```

→ `{"new":3,"reindexed":12,"touched":1,"unchanged":13280,"deleted":0,…}` in ~250ms on 13k files.

```json
{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"index","arguments":{}}}
```

Full rebuild — wipes the SQLite store and reindexes. Use when extractor rules change.

---

## 10. Errors

Missing args, unknown tool, parse failures → JSON-RPC error response:

```json
{"jsonrpc":"2.0","id":99,
 "error":{"code":-32603,"message":"tool error: missing arg: name"}}
```

| code     | meaning                       |
|----------|-------------------------------|
| `-32700` | parse error (malformed JSON)  |
| `-32601` | method not found              |
| `-32603` | tool error (missing arg, etc) |

---

## 11. Session walkthrough — one round trip

A full minimal session you can paste directly into a stdio terminal:

```text
> {"jsonrpc":"2.0","id":1,"method":"initialize"}
< {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05",…}}

> {"jsonrpc":"2.0","method":"notifications/initialized"}

> {"jsonrpc":"2.0","id":2,"method":"tools/call",
   "params":{"name":"refs","arguments":{"name":"Assessment","mode":"count"}}}
< {"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"count\":446}"}]}}
```

Each request is one line of newline-delimited JSON. Each response is one line back. EOF on stdin shuts the server down cleanly.

---

## 12. When to use `rg` / `fd` / `jq` instead

The MCP server only handles **code-shape** questions. For everything else, fall back
to modern shell tools:

| Question                                   | Reach for                  |
|--------------------------------------------|----------------------------|
| Free-text search in markdown / yaml / json | `rg "pattern" path/`       |
| Filename glob / by age / non-code files    | `fd PATTERN path/`         |
| Reshape crabcc JSON output                 | `crabcc … | jq …`          |
| Project a few fields from crabcc result    | `jq -r '.[].file'`         |

**Never reach for `grep -rn` or `find . -name`** on a real repo — they walk
`node_modules/`, `.git/`, `tmp/`. `rg` and `fd` are gitignore-aware by default.

The MCP server returns the same JSON the CLI prints. After the agent receives a
`tools/call` response, it can pipe that JSON through `jq` via a follow-up Bash call
for filtering, projection, or grouping — no need to re-query crabcc.

---

## 13. Why MCP, not CLI?

| Concern                  | CLI subprocess            | MCP                        |
|--------------------------|---------------------------|----------------------------|
| Startup overhead         | ~5–15ms per call          | one-time process spawn     |
| Stdout parsing           | agent parses `crabcc` JSON| MCP harness parses for you |
| Concurrent calls         | spawns more processes     | persistent session         |
| Tool discoverability     | via skill / docs          | via `tools/list`           |
| Permission model         | per-Bash-invocation prompt| one MCP-server approval    |

For agents that make many lookups in one session, MCP wins on both latency and approval-prompt count.

---

## 14. Cheatsheet

| Want                         | tools/call params                                                |
|------------------------------|------------------------------------------------------------------|
| `Foo` defined where?         | `{"name":"sym","arguments":{"name":"Foo"}}`                      |
| All callers of `bar`         | `{"name":"callers","arguments":{"name":"bar"}}`                  |
| How many callers of `bar`    | `{"name":"callers","arguments":{"name":"bar","mode":"count"}}`   |
| Which files reference `Baz`  | `{"name":"refs","arguments":{"name":"Baz","mode":"files"}}`      |
| Outline of one file          | `{"name":"outline","arguments":{"file":"app/x.rb"}}`             |
| All `.rb` under `app/models` | `{"name":"files","arguments":{"under":"app/models","ext":"rb"}}` |
| Misremembered name           | `{"name":"fuzzy","arguments":{"query":"Asseessment"}}`           |
| Prefix search                | `{"name":"prefix","arguments":{"query":"getUser"}}`              |
| Incremental refresh          | `{"name":"refresh","arguments":{}}`                              |
