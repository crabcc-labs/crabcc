# Copilot cloud agent — MCP configuration

Paste the contents of `mcp.json` into **Settings → Environments → copilot → MCP configuration** in the GitHub UI (the field shown in the screenshot).

## Required environment secrets (set in the copilot environment)

| Secret | Value |
|--------|-------|
| `CRABCC_MCP_URL` | Public HTTPS URL of `crabcc serve` (see below) |
| `CRABCC_AUTH_TOKEN` | `ANTHROPIC_AUTH_TOKEN` from `free-claude-code/.env` |

## Expose crabcc serve via HTTPS (private repo, self-hosted)

Copilot cloud agent requires **HTTPS** — stdio MCP is not supported.

```bash
# Option A — cloudflared (free, stable tunnel)
crabcc serve &          # starts on :8090
cloudflared tunnel --url http://localhost:8090
# → Outputs: https://random.trycloudflare.com
# Set CRABCC_MCP_URL=https://random.trycloudflare.com

# Option B — tailscale funnel
crabcc serve &
tailscale funnel 8090   # requires Tailscale account
# Set CRABCC_MCP_URL=https://<machine>.tailnet.ts.net

# Option C — ngrok
ngrok http 8090
# Set CRABCC_MCP_URL=https://xxxx.ngrok.io
```

## Private repo note

Yes, you can keep the repo private and still use Copilot cloud agent MCP:
- The MCP server URL just needs to be reachable from GitHub's runners
- Cloudflared/tailscale tunnels work fine with private repos
- The `CRABCC_AUTH_TOKEN` secret is encrypted in the GitHub copilot environment

## MCP tools exposed by crabcc serve

| Tool | Description |
|------|-------------|
| `crabcc.sym` | Symbol definition lookup |
| `crabcc.refs` | Find all references |
| `crabcc.callers` | Find callers of a function |
| `crabcc.outline` | Outline a file |
| `crabcc.fuzzy` | Fuzzy symbol search |
| `crabcc.memory.search` | Search AI memory drawers |
| `crabcc.memory.remember` | Save a memory drawer |
| `crabcc.graph` | Call graph queries |
