# MCP wire protocol — JSON-RPC 2.0 walkthrough

Every interaction with `crabcc --mcp` is one line of newline-delimited JSON-RPC 2.0
in, one line out. EOF on stdin shuts the server down.

## 1. Initialize handshake

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

## 2. List tools

```json
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
```

Returns an array of 9 tools with JSON-Schema input definitions. Excerpt — `refs`:

```json
{
  "name":"refs",
  "description":"Find every identifier reference to `name` across the indexed repo. …",
  "inputSchema":{
    "type":"object",
    "properties":{
      "name":  {"type":"string","description":"symbol name"},
      "mode":  {"type":"string","enum":["hits","files","count"], "description":"…"},
      "limit": {"type":"integer","description":"Cap result size."}
    },
    "required":["name"]
  }
}
```

## 3. tools/call — `sym`

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call",
 "params":{"name":"sym","arguments":{"name":"Assessment"}}}
```

**Response** — text content block whose body is the same JSON the CLI emits:

```json
{"jsonrpc":"2.0","id":3,"result":{"content":[
  {"type":"text","text":"[{\"name\":\"Assessment\",\"kind\":\"class\",…}]"}
]}}
```

## 4. tools/call — `refs` with mode

### Just count

```json
{"jsonrpc":"2.0","id":4,"method":"tools/call",
 "params":{"name":"refs","arguments":{"name":"Assessment","mode":"count"}}}
```

→ `{"count":446}`

### Just file paths, capped

```json
{"jsonrpc":"2.0","id":5,"method":"tools/call",
 "params":{"name":"refs","arguments":{"name":"Assessment","mode":"files","limit":10}}}
```

→ `{"files":["app/builders/...","app/models/...", …]}`

### Full hits, capped

```json
{"jsonrpc":"2.0","id":6,"method":"tools/call",
 "params":{"name":"callers","arguments":{"name":"find_by","limit":5}}}
```

## 5. tools/call — `outline`

```json
{"jsonrpc":"2.0","id":7,"method":"tools/call",
 "params":{"name":"outline","arguments":{"file":"app/models/user.rb"}}}
```

## 6. tools/call — `files`

```json
{"jsonrpc":"2.0","id":8,"method":"tools/call",
 "params":{"name":"files","arguments":{"under":"app/models","ext":"rb","limit":5}}}
```

## 7. tools/call — `fuzzy` / `prefix`

```json
{"jsonrpc":"2.0","id":9,"method":"tools/call",
 "params":{"name":"fuzzy","arguments":{"query":"Authentcator"}}}
```

```json
{"jsonrpc":"2.0","id":10,"method":"tools/call",
 "params":{"name":"prefix","arguments":{"query":"getUser"}}}
```

## 8. tools/call — `index` / `refresh`

```json
{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"refresh","arguments":{}}}
```

→ `{"new":3,"reindexed":12,"touched":1,"unchanged":13280,"deleted":0,…}`

## 9. Errors

Missing args, unknown tool, parse failures → JSON-RPC error response:

```json
{"jsonrpc":"2.0","id":99,
 "error":{"code":-32603,"message":"tool error: missing arg: name"}}
```

| Code     | Meaning                       |
|----------|-------------------------------|
| `-32700` | Parse error (malformed JSON)  |
| `-32601` | Method not found              |
| `-32603` | Tool error (missing arg, etc) |

## 10. Full minimal session

```text
> {"jsonrpc":"2.0","id":1,"method":"initialize"}
< {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05",…}}

> {"jsonrpc":"2.0","method":"notifications/initialized"}

> {"jsonrpc":"2.0","id":2,"method":"tools/call",
   "params":{"name":"refs","arguments":{"name":"Assessment","mode":"count"}}}
< {"jsonrpc":"2.0","id":2,"result":{"content":[
    {"type":"text","text":"{\"count\":446}"}
  ]}}
```

## 11. Notes

- All `tools/call` responses wrap the result in a `content` array with one text
  block whose body is the same JSON the CLI prints. That way the agent gets the
  same payload whether it talks to crabcc via subprocess or MCP.
- `notifications/initialized` is one-way (no `id`, no response). The MCP spec
  expects clients to send it after `initialize`.
- Concurrency: the server processes one request at a time per stdio session.
  For parallel calls, run multiple servers (or use the CLI which spawns processes).
