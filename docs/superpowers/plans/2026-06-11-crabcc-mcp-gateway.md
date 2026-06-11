# crabcc MCP Gateway — NixOS Module Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Produce `install/nixos/crabcc-gateway.nix` — a single NixOS module that deploys crabcc's HTTP MCP server behind Traefik (TLS + security headers + rate limiting) and oauth2-proxy (GitHub OAuth2 + bearer token passthrough).

**Architecture:** Three loopback-only services — crabcc on `:3000`, oauth2-proxy on `:4180`, Traefik on `:80`/`:443`. Traefik terminates TLS via Let's Encrypt ACME and uses oauth2-proxy as a ForwardAuth middleware so every request is authenticated before it reaches crabcc. Defense-in-depth: crabcc also requires a `MCP_AUTH_TOKEN` bearer token loaded from a secrets file.

**Tech Stack:** NixOS module system (`lib.mkOption`, `lib.mkIf`), `services.traefik` (nixpkgs), `services.oauth2-proxy` (nixpkgs), systemd hardening options.

---

## File Structure

```
install/nixos/
  crabcc-gateway.nix   # CREATE — single NixOS module with all options and service config
```

One file. No helper modules. The module wires `systemd.services.crabcc-mcp`, `services.oauth2-proxy`, and `services.traefik` under a single `services.crabcc-gateway` option namespace.

---

## Task 1: Create module skeleton with all options

**Files:**
- Create: `install/nixos/crabcc-gateway.nix`

- [ ] **Step 1: Create `install/nixos/` directory**

```bash
mkdir -p install/nixos
```

- [ ] **Step 2: Write the module skeleton with options**

Create `install/nixos/crabcc-gateway.nix`:

```nix
{ config, lib, pkgs, ... }:

with lib;

let
  cfg = config.services.crabcc-gateway;
in {
  options.services.crabcc-gateway = {
    enable = mkEnableOption "crabcc MCP gateway (Traefik + oauth2-proxy + crabcc)";

    package = mkOption {
      type = types.package;
      default = pkgs.crabcc;
      defaultText = literalExpression "pkgs.crabcc";
      description = "The crabcc package. Must be in pkgs (add via overlay in your flake).";
    };

    domain = mkOption {
      type = types.str;
      example = "mcp.example.com";
      description = "Public FQDN for the MCP gateway, e.g. mcp.example.com.";
    };

    repoRoot = mkOption {
      type = types.path;
      example = "/srv/repos/myrepo";
      description = ''
        Absolute path to the repository crabcc indexes. The crabcc-mcp system
        user must be able to write to {repoRoot}/.crabcc/ (for the SQLite index).
        Run: chown -R crabcc-mcp:crabcc-mcp {repoRoot}/.crabcc
      '';
    };

    httpPort = mkOption {
      type = types.port;
      default = 3000;
      description = "Loopback port for crabcc HTTP MCP server.";
    };

    oauthProxyPort = mkOption {
      type = types.port;
      default = 4180;
      description = "Loopback port for oauth2-proxy ForwardAuth service.";
    };

    githubClientId = mkOption {
      type = types.str;
      description = "GitHub OAuth app Client ID.";
    };

    githubClientSecretFile = mkOption {
      type = types.path;
      description = "Path to file containing the GitHub OAuth client secret (plain text, single line). Compatible with sops-nix and agenix.";
      example = "/run/secrets/github-client-secret";
    };

    cookieSecretFile = mkOption {
      type = types.path;
      description = ''
        Path to file containing the oauth2-proxy cookie secret.
        Generate with: python3 -c "import secrets; print(secrets.token_hex(16))"
        Must be 16 or 32 bytes when decoded.
      '';
      example = "/run/secrets/cookie-secret";
    };

    mcpTokenFile = mkOption {
      type = types.path;
      description = ''
        Path to file loaded as crabcc's EnvironmentFile.
        File must contain exactly one line: MCP_AUTH_TOKEN=<value>
        Generate with: echo "MCP_AUTH_TOKEN=$(openssl rand -hex 16)" > /run/secrets/mcp-token
      '';
      example = "/run/secrets/mcp-token";
    };

    allowedGitHubUsers = mkOption {
      type = types.listOf types.str;
      default = [];
      example = [ "your-gh-handle" ];
      description = ''
        GitHub usernames allowed access. Takes precedence over allowedGitHubOrg.
        If both are empty, any authenticated GitHub user is allowed.
      '';
    };

    allowedGitHubOrg = mkOption {
      type = types.str;
      default = "";
      example = "my-org";
      description = "GitHub org for org-level access. Used when allowedGitHubUsers is empty.";
    };

    acmeEmail = mkOption {
      type = types.str;
      description = "Email address for Let's Encrypt ACME registration.";
    };
  };

  config = mkIf cfg.enable {
    # Tasks 2-6 fill this in
  };
}
```

- [ ] **Step 3: Verify the file parses**

```bash
nix-instantiate --parse install/nixos/crabcc-gateway.nix
```

Expected: prints the parsed Nix AST with no errors.

- [ ] **Step 4: Commit**

```bash
git add install/nixos/crabcc-gateway.nix
git commit -m "feat(nixos): crabcc-gateway module skeleton + options"
```

---

## Task 2: System user + crabcc systemd service

**Files:**
- Modify: `install/nixos/crabcc-gateway.nix` — add inside `config = mkIf cfg.enable { ... }`

- [ ] **Step 1: Replace the empty `config` block with the system user + service**

Replace:
```nix
  config = mkIf cfg.enable {
    # Tasks 2-6 fill this in
  };
```

With:
```nix
  config = mkIf cfg.enable {

    # ── System user (fixed UID so repoRoot/.crabcc stays writable) ──────────
    users.users.crabcc-mcp = {
      isSystemUser = true;
      group = "crabcc-mcp";
      description = "crabcc MCP gateway service user";
    };
    users.groups.crabcc-mcp = {};

    # ── crabcc HTTP MCP server ───────────────────────────────────────────────
    systemd.services.crabcc-mcp = {
      description = "crabcc MCP HTTP server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      serviceConfig = {
        User = "crabcc-mcp";
        Group = "crabcc-mcp";
        ExecStart = "${cfg.package}/bin/crabcc --mcp-http 127.0.0.1:${toString cfg.httpPort} --root ${cfg.repoRoot}";
        EnvironmentFile = cfg.mcpTokenFile;

        # Hardening
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        CapabilityBoundingSet = "";
        SystemCallFilter = [ "@system-service" ];
        RestrictAddressFamilies = [ "AF_UNIX" "AF_INET" "AF_INET6" ];
        # BindPaths (rw) so crabcc can write .crabcc/index.db inside repoRoot
        BindPaths = [ (toString cfg.repoRoot) ];

        Restart = "on-failure";
        RestartSec = "5s";
      };
    };

    # Tasks 3-6 continue here (oauth2-proxy, traefik, firewall)
  };
```

- [ ] **Step 2: Parse check**

```bash
nix-instantiate --parse install/nixos/crabcc-gateway.nix
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add install/nixos/crabcc-gateway.nix
git commit -m "feat(nixos): add crabcc-mcp systemd unit with hardening"
```

---

## Task 3: oauth2-proxy configuration

**Files:**
- Modify: `install/nixos/crabcc-gateway.nix` — add inside `config = mkIf cfg.enable { ... }` after the crabcc service

**Note on ForwardAuth mode:** oauth2-proxy here acts as an auth decision service only — it returns 200 (authenticated) or 302 (redirect to GitHub). Traefik handles the actual proxying to crabcc. So `upstream = []`.

- [ ] **Step 1: Add oauth2-proxy config after the crabcc service block**

Remove the comment `# Tasks 3-6 continue here (oauth2-proxy, traefik, firewall)` and replace it with:

```nix
    # ── oauth2-proxy (GitHub OAuth2, ForwardAuth mode) ──────────────────────
    services.oauth2-proxy = {
      enable = true;
      provider = "github";
      clientID = cfg.githubClientId;
      clientSecretFile = cfg.githubClientSecretFile;

      # ForwardAuth mode: Traefik proxies to crabcc; oauth2-proxy only decides auth
      upstream = [];
      redirectURL = "https://${cfg.domain}/oauth2/callback";
      httpAddress = "http://127.0.0.1:${toString cfg.oauthProxyPort}";

      cookie = {
        secretFile = cfg.cookieSecretFile;
        httpOnly = true;
      };

      # Running behind Traefik — trust X-Forwarded-* headers
      reverseProxy = true;

      # Forward auth user identity headers to crabcc
      setXauthrequest = true;

      extraConfig = mkMerge [
        {
          # 4h session lifetime; SameSite=lax
          cookie-expire    = "4h";
          cookie-samesite  = "lax";
          # MCP clients (Claude Code) authenticate with Bearer token directly,
          # bypassing the browser OAuth flow
          skip-jwt-bearer-tokens = "true";
        }
        (mkIf (cfg.allowedGitHubUsers != []) {
          github-user = concatStringsSep "," cfg.allowedGitHubUsers;
        })
        (mkIf (cfg.allowedGitHubOrg != "") {
          github-org = cfg.allowedGitHubOrg;
        })
      ];
    };

    # Tasks 4-6 continue here (traefik, firewall)
```

- [ ] **Step 2: Parse check**

```bash
nix-instantiate --parse install/nixos/crabcc-gateway.nix
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add install/nixos/crabcc-gateway.nix
git commit -m "feat(nixos): add oauth2-proxy GitHub ForwardAuth config"
```

---

## Task 4: Traefik static config (ACME + entrypoints)

**Files:**
- Modify: `install/nixos/crabcc-gateway.nix` — add Traefik static config

- [ ] **Step 1: Add Traefik service with static config**

Remove `# Tasks 4-6 continue here (traefik, firewall)` and add:

```nix
    # ── Traefik (TLS termination, ForwardAuth, security middleware) ─────────
    services.traefik = {
      enable = true;

      staticConfigOptions = {
        entryPoints = {
          web = {
            address = ":80";
          };
          websecure = {
            address = ":443";
          };
        };

        certificatesResolvers = {
          letsencrypt = {
            acme = {
              email = cfg.acmeEmail;
              storage = "/var/lib/traefik/acme.json";
              tlsChallenge = {};
            };
          };
        };

        # Disable the Traefik dashboard — no admin UI exposed
        api = {
          insecure = false;
          dashboard = false;
        };

        log = {
          level = "WARN";
        };

        # Log every request for audit trail
        accessLog = {};
      };

      # dynamicConfigOptions added in Task 5
    };

    # Tasks 5-6 continue here (traefik dynamic config, firewall)
```

- [ ] **Step 2: Parse check**

```bash
nix-instantiate --parse install/nixos/crabcc-gateway.nix
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add install/nixos/crabcc-gateway.nix
git commit -m "feat(nixos): add Traefik static config with ACME + entrypoints"
```

---

## Task 5: Traefik dynamic config (middlewares, router, service, TLS)

**Files:**
- Modify: `install/nixos/crabcc-gateway.nix` — add `dynamicConfigOptions` to the Traefik service

- [ ] **Step 1: Add `dynamicConfigOptions` inside the `services.traefik` block**

The `services.traefik` block currently ends with `# dynamicConfigOptions added in Task 5`. Add the `dynamicConfigOptions` attribute before the closing `};` of `services.traefik`:

```nix
      dynamicConfigOptions = {
        http = {
          middlewares = {
            # ── Auth: forward every request to oauth2-proxy for auth check ──
            "forward-auth" = {
              forwardAuth = {
                address = "http://127.0.0.1:${toString cfg.oauthProxyPort}";
                trustForwardHeader = true;
                authResponseHeaders = [
                  "X-Auth-Request-User"
                  "X-Auth-Request-Email"
                ];
              };
            };

            # ── Security headers ────────────────────────────────────────────
            "security-headers" = {
              headers = {
                stsSeconds = 31536000;
                stsIncludeSubdomains = true;
                contentTypeNosniff = true;
                frameDeny = true;
                referrerPolicy = "strict-origin-when-cross-origin";
              };
            };

            # ── Rate limiting: 100 req/10s per source IP, burst 20 ─────────
            "rate-limit" = {
              rateLimit = {
                average = 100;
                burst = 20;
                period = "10s";
              };
            };

            # ── HTTP → HTTPS redirect ───────────────────────────────────────
            "https-redirect" = {
              redirectScheme = {
                scheme = "https";
                permanent = true;
              };
            };
          };

          routers = {
            # Redirect plain HTTP to HTTPS
            "http-redirect" = {
              rule = "Host(`${cfg.domain}`)";
              entryPoints = [ "web" ];
              middlewares = [ "https-redirect" ];
              service = "noop@internal";
            };

            # Main MCP router on HTTPS — auth + security headers + rate limit
            "crabcc-mcp" = {
              rule = "Host(`${cfg.domain}`)";
              entryPoints = [ "websecure" ];
              middlewares = [ "forward-auth" "security-headers" "rate-limit" ];
              service = "crabcc-mcp";
              tls = {
                certResolver = "letsencrypt";
              };
            };
          };

          services = {
            "crabcc-mcp" = {
              loadBalancer = {
                servers = [
                  { url = "http://127.0.0.1:${toString cfg.httpPort}"; }
                ];
              };
            };
          };
        };

        # TLS: minimum TLS 1.2, strong cipher suites only
        tls = {
          options = {
            "default" = {
              minVersion = "VersionTLS12";
              cipherSuites = [
                "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256"
                "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384"
                "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256"
                "TLS_AES_128_GCM_SHA256"
                "TLS_AES_256_GCM_SHA384"
                "TLS_CHACHA20_POLY1305_SHA256"
              ];
            };
          };
        };
      };
```

Also remove the `# Tasks 5-6 continue here` comment.

- [ ] **Step 2: Parse check**

```bash
nix-instantiate --parse install/nixos/crabcc-gateway.nix
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add install/nixos/crabcc-gateway.nix
git commit -m "feat(nixos): add Traefik dynamic config — middlewares, router, TLS"
```

---

## Task 6: Firewall + close the config block

**Files:**
- Modify: `install/nixos/crabcc-gateway.nix` — add firewall rule, remove remaining comment

- [ ] **Step 1: Add firewall rule after the `services.traefik` closing `};`**

```nix
    # ── Firewall: only expose 80 (redirect) and 443 (MCP HTTPS) ────────────
    networking.firewall.allowedTCPPorts = [ 80 443 ];
```

The `config = mkIf cfg.enable { ... };` closing brace should now close cleanly — no remaining placeholder comments.

- [ ] **Step 2: Parse check**

```bash
nix-instantiate --parse install/nixos/crabcc-gateway.nix
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add install/nixos/crabcc-gateway.nix
git commit -m "feat(nixos): add firewall rules + complete module"
```

---

## Task 7: Eval smoke test

Verify the module evaluates without type errors against a minimal NixOS system. This catches wrong option names and type mismatches before deploy.

**Files:**
- No new files — run inline eval.

- [ ] **Step 1: Run the eval test**

```bash
nix-instantiate --eval --strict --json -E '
  let
    pkgs   = import <nixpkgs> {};
    lib    = pkgs.lib;
    system = lib.nixosSystem {
      inherit (pkgs) system;
      modules = [
        (import ./install/nixos/crabcc-gateway.nix)
        {
          services.crabcc-gateway = {
            enable                 = true;
            package                = pkgs.hello;      # stand-in; any package works for eval
            domain                 = "mcp.example.com";
            repoRoot               = /srv/repos/test;
            githubClientId         = "test-id";
            githubClientSecretFile = /tmp/test-secret;
            cookieSecretFile       = /tmp/test-cookie;
            mcpTokenFile           = /tmp/test-token;
            allowedGitHubUsers     = [ "testuser" ];
            acmeEmail              = "test@example.com";
          };
          boot.loader.grub.enable    = false;
          fileSystems."/"            = { device = "none"; fsType = "tmpfs"; };
        }
      ];
    };
  in
    builtins.attrNames system.config.systemd.services
' 2>&1 | grep -E '(crabcc-mcp|error|Error)' | head -20
```

Expected: output includes `"crabcc-mcp"` and no `error` lines.

If you see "attribute 'crabcc' missing" for the package, that's expected — it just means `pkgs.hello` stand-in was used correctly but you'll want to verify with your actual flake.

- [ ] **Step 2: Verify oauth2-proxy and Traefik services are present**

```bash
nix-instantiate --eval --strict --json -E '
  let
    pkgs   = import <nixpkgs> {};
    lib    = pkgs.lib;
    system = lib.nixosSystem {
      inherit (pkgs) system;
      modules = [
        (import ./install/nixos/crabcc-gateway.nix)
        {
          services.crabcc-gateway = {
            enable                 = true;
            package                = pkgs.hello;
            domain                 = "mcp.example.com";
            repoRoot               = /srv/repos/test;
            githubClientId         = "test-id";
            githubClientSecretFile = /tmp/test-secret;
            cookieSecretFile       = /tmp/test-cookie;
            mcpTokenFile           = /tmp/test-token;
            allowedGitHubUsers     = [ "testuser" ];
            acmeEmail              = "test@example.com";
          };
          boot.loader.grub.enable    = false;
          fileSystems."/"            = { device = "none"; fsType = "tmpfs"; };
        }
      ];
    };
  in {
    hasCrabcc      = system.config.systemd.services ? crabcc-mcp;
    hasOauthProxy  = system.config.services.oauth2-proxy.enable;
    hasTraefik     = system.config.services.traefik.enable;
    firewallPorts  = system.config.networking.firewall.allowedTCPPorts;
    crabccExecStart = system.config.systemd.services.crabcc-mcp.serviceConfig.ExecStart;
  }
' 2>&1
```

Expected output (JSON):
```json
{
  "hasCrabcc": true,
  "hasOauthProxy": true,
  "hasTraefik": true,
  "firewallPorts": [80, 443],
  "crabccExecStart": "/nix/store/...-hello-.../bin/hello --mcp-http 127.0.0.1:3000 --root /srv/repos/test"
}
```

(ExecStart will show `hello` binary as the stand-in package; `--mcp-http` and `--root` args should be correct.)

- [ ] **Step 3: Commit**

```bash
git add install/nixos/crabcc-gateway.nix
git commit -m "test(nixos): verify crabcc-gateway module evals correctly"
```

---

## Task 8: Hetzner deploy checklist

Pre-deploy steps. Not automated — run manually on the target server.

- [ ] **Step 1: Generate secrets on the Hetzner server**

```bash
# GitHub OAuth client secret (copy from GitHub app settings, paste here)
echo -n "your-client-secret" | install -m 600 /dev/stdin /run/secrets/github-client-secret

# oauth2-proxy cookie secret (must be 16 or 32 bytes decoded)
python3 -c "import secrets; print(secrets.token_hex(16))" \
  | install -m 600 /dev/stdin /run/secrets/cookie-secret

# crabcc bearer token (used in Claude Code config as Authorization: Bearer)
echo "MCP_AUTH_TOKEN=$(openssl rand -hex 16)" \
  | install -m 600 /dev/stdin /run/secrets/mcp-token

# Save the MCP_AUTH_TOKEN value — you'll need it for Claude Code config
grep MCP_AUTH_TOKEN /run/secrets/mcp-token
```

Wrap these paths with sops-nix or agenix if you want them to survive reboots declaratively.

- [ ] **Step 2: Set up DNS**

Point an A record for `mcp.yourdomain.com` to the Hetzner server's public IP. Verify:

```bash
dig +short mcp.yourdomain.com     # should return the Hetzner IP
```

- [ ] **Step 3: Create GitHub OAuth app**

In GitHub → Settings → Developer settings → OAuth Apps → New OAuth App:
- Homepage URL: `https://mcp.yourdomain.com`
- Authorization callback URL: `https://mcp.yourdomain.com/oauth2/callback`

Copy the Client ID and Client Secret into your NixOS config / secrets.

- [ ] **Step 4: Ensure repoRoot is writable by crabcc-mcp**

```bash
# The .crabcc/ directory (index storage) needs to be writable by the service user
mkdir -p /srv/repos/myrepo/.crabcc
chown -R crabcc-mcp:crabcc-mcp /srv/repos/myrepo/.crabcc
```

The repo source files themselves can stay owned by root/another user (BindPaths mounts rw, but crabcc only writes to `.crabcc/`).

- [ ] **Step 5: Add the module to your NixOS host config**

In your flake or configuration.nix:

```nix
imports = [ ./crabcc-gateway.nix ];  # adjust path

services.crabcc-gateway = {
  enable                 = true;
  domain                 = "mcp.yourdomain.com";
  repoRoot               = /srv/repos/myrepo;
  package                = pkgs.crabcc;    # from your flake overlay
  githubClientId         = "Ov23ct...";    # from GitHub OAuth app
  githubClientSecretFile = /run/secrets/github-client-secret;
  cookieSecretFile       = /run/secrets/cookie-secret;
  mcpTokenFile           = /run/secrets/mcp-token;
  allowedGitHubUsers     = [ "your-gh-handle" ];
  acmeEmail              = "you@example.com";
};
```

- [ ] **Step 6: Rebuild and verify services are running**

```bash
nixos-rebuild switch
systemctl status crabcc-mcp oauth2-proxy traefik
```

Expected: all three `active (running)`.

- [ ] **Step 7: Verify TLS + auth end-to-end**

```bash
# Should redirect to GitHub OAuth login (302)
curl -v https://mcp.yourdomain.com/

# Should work with the bearer token (200 JSON-RPC response)
TOKEN=$(grep MCP_AUTH_TOKEN /run/secrets/mcp-token | cut -d= -f2)
curl -s -X POST https://mcp.yourdomain.com/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}' \
  | jq .result.serverInfo
```

Expected: JSON with `{"name": "crabcc", "version": "..."}`.

- [ ] **Step 8: Configure Claude Code**

In your Claude Code MCP config (`.claude/mcp.json` or global `~/.claude.json`):

```json
{
  "mcpServers": {
    "crabcc": {
      "url": "https://mcp.yourdomain.com",
      "headers": {
        "Authorization": "Bearer <MCP_AUTH_TOKEN value from /run/secrets/mcp-token>"
      }
    }
  }
}
```

- [ ] **Step 9: Final commit**

```bash
git add install/nixos/crabcc-gateway.nix
git commit -m "feat(nixos): crabcc MCP gateway module — complete implementation"
```

---

## Self-review

**Spec coverage check:**

| Spec requirement | Task |
|---|---|
| Single NixOS module at `install/nixos/crabcc-gateway.nix` | Task 1 |
| `services.crabcc-gateway.*` options with file-based secrets | Task 1 |
| crabcc systemd unit with `--mcp-http` + `EnvironmentFile` | Task 2 |
| Systemd hardening (NoNewPrivileges, ProtectSystem, etc.) | Task 2 |
| Fixed system user (not DynamicUser — repoRoot needs write) | Task 2 |
| oauth2-proxy GitHub provider, ForwardAuth mode | Task 3 |
| Bearer token passthrough (`skip-jwt-bearer-tokens`) | Task 3 |
| Cookie: HttpOnly, 4h expiry, SameSite=lax | Task 3 |
| GitHub user/org allowlist | Task 3 |
| Traefik ACME + entrypoints | Task 4 |
| HTTP→HTTPS redirect | Task 5 |
| ForwardAuth middleware | Task 5 |
| Security headers (HSTS, X-Frame, X-Content-Type, Referrer) | Task 5 |
| Rate limiting (100 req/10s, burst 20) | Task 5 |
| TLS 1.2 minimum + cipher list | Task 5 |
| Traefik dashboard disabled | Task 4 |
| Firewall 80 + 443 only | Task 6 |
| No secrets in Nix store | Tasks 1, 3 (secretFile options use systemd credentials) |
| Defense-in-depth: crabcc also requires MCP_AUTH_TOKEN | Task 2 |

**Spec delta (intentional deviations noted):**
- Spec said `DynamicUser = true`; plan uses fixed `users.users.crabcc-mcp` because `BindPaths` + dynamic UID makes repoRoot write access fragile. The hardening level is equivalent.
- Spec said `BindReadOnlyPaths`; plan uses `BindPaths` (rw) because crabcc writes `.crabcc/index.db` inside repoRoot.
