# crabcc MCP Gateway — NixOS Deploy Module

**Date:** 2026-06-11
**Status:** Approved, pending implementation plan

---

## Problem

`crabcc --mcp-http` exposes a bare HTTP JSON-RPC endpoint. Running it on a
public Hetzner VPS without a security layer means no TLS, no auth, and no rate
limiting. The selfhostedmcp.com reference architecture describes the right
security posture for self-hosted MCP servers; this spec translates it into a
single NixOS module for crabcc.

## Scope

A single NixOS module (`install/nixos/crabcc-gateway.nix`) that:
- Deploys crabcc's existing HTTP MCP server (`--mcp-http`) as a systemd unit
- Puts it behind Traefik (TLS + security headers + rate limiting)
- Gates access with oauth2-proxy (GitHub OAuth2)
- Hardens the systemd unit against privilege escalation
- Exposes exactly the options needed — nothing more

No new Rust code. No Docker containers. All services from nixpkgs.

## Architecture

```
Internet
  |
  | 80/443 (firewall: only these open externally)
  v
[Traefik]  TLS via Let's Encrypt ACME
  |  ForwardAuth middleware -> oauth2-proxy
  |  Security-headers middleware
  |  Rate-limit middleware (100 req/10s, burst 20)
  v
[oauth2-proxy :4180]  GitHub OAuth2; Bearer token passthrough
  |  (bound to 127.0.0.1)
  v
[crabcc :3000]  --mcp-http 127.0.0.1:3000
  |  (bound to 127.0.0.1; also requires MCP_AUTH_TOKEN)
  v
  .crabcc/index.db  (repoRoot on disk)
```

WireGuard is not used: crabcc is on the same host as Traefik, so no
private tunnel is needed.

## NixOS Module

**File:** `install/nixos/crabcc-gateway.nix`

### Options

| Option | Type | Default | Description |
|---|---|---|---|
| `enable` | bool | false | Enable the gateway stack |
| `domain` | string | — | Public FQDN, e.g. `mcp.example.com` |
| `repoRoot` | path | — | Directory crabcc indexes |
| `githubClientId` | string | — | GitHub OAuth app client ID |
| `githubClientSecretFile` | path | — | File containing the client secret (sops-nix / agenix compatible) |
| `cookieSecretFile` | path | — | File containing 32-byte random cookie secret |
| `mcpTokenFile` | path | — | File containing the bearer token for crabcc (defense-in-depth) |
| `allowedGitHubUsers` | list of string | `[]` | GitHub usernames allowed access; empty = org-level only |
| `allowedGitHubOrg` | string | `""` | GitHub org; used if allowedGitHubUsers is empty |
| `httpPort` | int | 3000 | crabcc internal bind port |
| `oauthProxyPort` | int | 4180 | oauth2-proxy internal bind port |
| `acmeEmail` | string | — | Let's Encrypt contact address |

All secret options accept file paths so no secret material ever lands in the
Nix store.

### Services configured

**`systemd.services.crabcc-mcp`**
- ExecStart: `crabcc --mcp-http 127.0.0.1:{httpPort} --root {repoRoot}`
- Environment: `MCP_AUTH_TOKEN` loaded via `EnvironmentFile` from `mcpTokenFile`
  (file format: `MCP_AUTH_TOKEN=<value>`, one line)
- Hardening: `NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=strict`,
  `ProtectHome`, `CapabilityBoundingSet=""`, `SystemCallFilter=@system-service`,
  `BindReadOnlyPaths=[repoRoot]`
- `DynamicUser = true` (crabcc needs no persistent UID)
- Restart: `on-failure`

**`services.oauth2-proxy`** (nixpkgs module)
- `provider = "github"`
- Binds to `127.0.0.1:{oauthProxyPort}` in ForwardAuth mode (Traefik handles
  the actual upstream proxying; oauth2-proxy only returns 200/401/302)
- `redirect-url = "https://{domain}/oauth2/callback"`
- `cookie-secure = true`, `cookie-httponly = true`, `cookie-samesite = "lax"`,
  `cookie-expire = "4h"`
- `skip-jwt-bearer-tokens = true` — allows MCP clients (Claude Code) to
  authenticate with a Bearer token instead of going through the browser OAuth
  flow
- `github-user` or `github-org` set from `allowedGitHubUsers` / `allowedGitHubOrg`;
  if both are empty, any authenticated GitHub user is allowed
- `client-secret-file` and `cookie-secret-file` from the module options

**`services.traefik`** (nixpkgs module)
- Static config:
  - ACME via Let's Encrypt, `caServer = "https://acme-v02.api.letsencrypt.org/directory"`,
    storage at `/var/lib/traefik/acme.json`, `email = acmeEmail`
  - `api.insecure = false` (dashboard off)
  - Access log enabled
- Dynamic config (inline via `dynamicConfigOptions`):
  - **Middleware `forward-auth`**: `forwardAuth.address = http://127.0.0.1:{oauthProxyPort}`
  - **Middleware `security-headers`**: HSTS `max-age=31536000; includeSubDomains`,
    `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
    `Referrer-Policy: strict-origin-when-cross-origin`
  - **Middleware `rate-limit`**: `average = 100`, `burst = 20`, `period = "10s"`
  - **TLS options**: `minVersion = "VersionTLS12"`, strong cipher list
  - **HTTP redirect**: permanent redirect 80 -> 443
  - **Router `crabcc-mcp`**: `rule = "Host(\`{domain}\`)"`,
    middlewares = `[forward-auth, security-headers, rate-limit]`,
    service = `crabcc-mcp`, tls = certresolver `letsencrypt`
  - **Service `crabcc-mcp`**: `loadBalancer.servers[0].url = "http://127.0.0.1:{httpPort}"`

**`networking.firewall.allowedTCPPorts = [80 443]`**

## Security properties

| Threat | Mitigation |
|---|---|
| Unauthenticated access | GitHub OAuth gate via oauth2-proxy; bearer token for API clients |
| Token theft / session fixation | Short-lived cookies (4h), Secure+HttpOnly+SameSite |
| Secrets in Nix store | All secrets loaded from files at activation time |
| Privilege escalation in crabcc | DynamicUser + CapabilityBoundingSet="" + NoNewPrivileges |
| Direct access to crabcc/oauth2-proxy | Bound to 127.0.0.1; external firewall allows only 80/443 |
| Weak TLS / downgrade | TLS 1.2 minimum, strong ciphers, HSTS |
| DoS / abuse | Rate limit 100 req/10s, burst 20 |
| Misconfigured proxy leaking endpoint | Defense-in-depth: crabcc also requires MCP_AUTH_TOKEN |
| Traefik dashboard exposed | `api.insecure = false` |

## Secret bootstrap (one-time, not in module)

Before deploying, generate:
```bash
openssl rand -hex 16 > /run/secrets/mcp-token          # crabcc bearer token
python3 -c "import secrets; print(secrets.token_hex(16))" > /run/secrets/cookie-secret
```
Wrap with sops-nix or agenix as preferred. The module takes the decrypted file
paths.

## Usage

```nix
# In your NixOS host config:
imports = [ ./crabcc-gateway.nix ];

services.crabcc-gateway = {
  enable = true;
  domain = "mcp.example.com";
  repoRoot = "/srv/repos/myrepo";
  githubClientId = "Ov23ct...";
  githubClientSecretFile = config.sops.secrets.github-client-secret.path;
  cookieSecretFile = config.sops.secrets.cookie-secret.path;
  mcpTokenFile = config.sops.secrets.mcp-token.path;
  allowedGitHubUsers = [ "your-gh-handle" ];
  acmeEmail = "you@example.com";
};
```

Claude Code MCP config:
```json
{
  "mcpServers": {
    "crabcc": {
      "url": "https://mcp.example.com",
      "headers": { "Authorization": "Bearer <mcp-token>" }
    }
  }
}
```

## Out of scope

- WireGuard tunneling (same-host deployment)
- CrowdSec intrusion detection (can be layered on later via Traefik middleware)
- Multi-repo routing (single `repoRoot` for now)
- Automatic domain purchase / DNS configuration
