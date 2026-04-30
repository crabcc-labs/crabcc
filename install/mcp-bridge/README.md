# crabcc MCP bridge — supergateway

Bridges `crabcc --mcp` (stdio) to HTTP for Copilot cloud agent, remote clients,
and any host that can't run stdio MCP locally.

Uses [@modelcontextprotocol/supergateway](https://github.com/supermachineapp/supergateway).

## Architecture

```
Copilot cloud agent         supergateway :8091          crabcc
  (HTTP/Streamable)    ──►  --stdio wrapper        ──►  --mcp (stdio)
  GET /mcp             ◄──  translates frames      ◄──  JSON-RPC
  POST /mcp                 --streamableHttpPath         over stdin/stdout
                            /mcp
                            --healthEndpoint /healthz
                            --cors
```

## Transport modes

| Client | supergateway flags | Config `type` |
|--------|-------------------|----------------|
| GitHub Copilot cloud agent | `--outputTransport streamableHttp` | `"http"` |
| Claude.ai / legacy clients | `--outputTransport sse` | `"sse"` |
| WebSocket clients | `--outputTransport ws` | — |
| Reverse (remote SSE → local) | `--sse <url>` | — |

## Quick start

```bash
# Start bridge (Streamable HTTP, Copilot-compatible):
task mcp-serve-http

# Start bridge + tunnel in one command:
task mcp-tunnel

# Override port or root:
task mcp-serve-http PORT=9000 ROOT=/path/to/repo
```

## Supergateway flags — our usage

```
--stdio "crabcc --root . --mcp"
    Wraps the crabcc stdio MCP server. Each client connection spawns
    a fresh crabcc process (stateless by default).

--port 8091
    HTTP port. Keep separate from crabcc serve (:8090 dashboard).

--outputTransport streamableHttp
    Streamable HTTP (MCP 2025-03-26 spec). Required by Copilot cloud
    agent. Use `sse` for older Claude.ai or LiteLLM MCP proxy.

--streamableHttpPath /mcp
    POST/GET endpoint. Copilot sends tool calls here.

--healthEndpoint /healthz
    Returns "ok". Used by cloudflared health checks + monitoring.

--cors
    Allow all origins. Needed when the tunnel domain differs from the
    client's origin (Copilot's request origin is github.com).

--logLevel info
    Logs each tool call + response. Use `debug` for full JSON frames,
    `none` to silence in production.

--stateful  (optional)
    Keep the crabcc process alive between tool calls (same index session).
    Without this, each call is a fresh subprocess. Faster for repeated
    calls to the same repo; uses more memory.

--sessionTimeout 60000  (only with --stateful)
    Terminate idle sessions after 60 s. Prevents zombie crabcc processes.
```

## Copilot MCP config

Paste into **Settings → Environments → copilot → MCP configuration**:

```json
{
  "mcpServers": {
    "crabcc": {
      "type": "http",
      "url": "${{ secrets.CRABCC_MCP_URL }}/mcp"
    }
  }
}
```

Copilot environment secret:

| Secret | Value |
|--------|-------|
| `CRABCC_MCP_URL` | `https://xxxx.trycloudflare.com` (output of `task mcp-tunnel`) |

## Copilot allowlist

Add to **Settings → Environments → copilot → Custom allowlist**:

```
*.trycloudflare.com
```

For stable domains (tailscale/ngrok custom):
```
mcp.yourdomain.com
```

## Tunnel options

| Tool | Command | Notes |
|------|---------|-------|
| cloudflared | `cloudflared tunnel --url http://localhost:8091` | Free, ephemeral URL |
| tailscale funnel | `tailscale funnel 8091` | Stable URL if on Tailscale |
| ngrok | `ngrok http 8091` | Free tier: ephemeral |
| bore | `bore local 8091 --to bore.pub` | Open-source, self-hostable |

## Health check

```bash
curl http://localhost:8091/healthz     # → "ok"
curl https://YOUR_TUNNEL_URL/healthz   # → "ok" (verify tunnel is live)
```

## MCP tool smoke test

```bash
# List tools (Streamable HTTP):
curl -X POST http://localhost:8091/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'

# Call sym tool:
curl -X POST http://localhost:8091/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"sym","arguments":{"name":"Store"}}}'
```
