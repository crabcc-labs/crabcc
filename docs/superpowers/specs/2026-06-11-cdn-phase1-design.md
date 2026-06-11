# cdn.crabcc.app Phase 1 — Design Spec

> Phase 1: Origin + dual PoP, no Cloudflare Worker yet. Direct HTTPS access to
> `cdn.crabcc.app` is the test surface. HMAC token validation and AOP certs come
> in Phase 2.

**Date:** 2026-06-11
**Status:** Approved, pending implementation plan
**Repo:** `peterlodri-sec/__cdn`

---

## Goal

Deploy a working 2-node CDN stack:
- `dev-cx53` (100.105.72.88, Hetzner nbg1) — MinIO origin + PoP #1
- `ccx33-nbg1-1` (100.95.56.108, Hetzner nbg1) — PoP #2 (Ubuntu → NixOS via nixos-anywhere)

Both nodes managed as a Colmena fleet from `__cdn/nix/`.

---

## Architecture

```
HTTPS client
     │
     │ :443 TLS (SNI passthrough on dev-cx53 / direct on ccx33)
     ▼
[NGINX :8443] on each PoP
  cert: Cloudflare DNS-01 ACME for cdn.crabcc.app
  cache: /var/cache/nginx/cdn  (slice 1m, NVMe-backed)
     │
     │ cache miss → proxy_pass via tailscale0
     ▼
[MinIO :9000] on dev-cx53
  bound: 0.0.0.0 (firewall blocks 9000 from public, open on tailscale0 only)
  bucket: crabcc-indexes
     │
Invalidation:
  NATS subject: index.invalidated.<org>.<repo>.<branch>
  Subscriber on each PoP → ngx_cache_purge matching cache keys
```

**Traefik TCP passthrough on dev-cx53** (dev-cx53 already runs Traefik for MCP gateway):
- SNI `cdn.crabcc.app` → TCP passthrough → NGINX :8443
- SNI `mcp.yourdomain.com` → existing Layer 7 Traefik handling

**ccx33-nbg1-1** has no Traefik — NGINX binds :443 directly (no conflict).

---

## Fleet Structure

```
__cdn/nix/
  flake.nix              # inputs: nixpkgs + colmena + nixos-anywhere
  colmena.nix            # hive: dev-cx53 (origin+pop) + ccx33-nbg1-1 (pop)
  modules/
    cdn-origin.nix       # services.minio, firewall, bucket bootstrap
    cdn-pop.nix          # services.nginx, ACME, cache config, options
    cdn-traefik-patch.nix # Traefik tcp.router for SNI passthrough (dev-cx53 only)
    cdn-invalidator.nix  # systemd unit: NATS → ngx_cache_purge
```

---

## Module Options (`cdn-pop.nix`)

```nix
services.crabcc-cdn = {
  enable               = true;
  domain               = "cdn.crabcc.app";
  originUrl            = "http://100.105.72.88:9000";  # MinIO tailscale IP
  nginxPort            = 8443;                          # 443 on ccx33, 8443 on dev-cx53
  cacheDir             = "/var/cache/nginx/cdn";
  cacheSize            = "50g";
  cloudflareTokenFile  = /run/secrets/cloudflare-api-token;  # DNS-01 ACME
  natsUrl              = "nats://100.73.72.35:4222";
  natsBucket           = "crabcc-indexes";
};
```

---

## Component Specs

### cdn-origin.nix (dev-cx53 only)

- `services.minio.enable = true`
- `services.minio.dataDir = ["/var/lib/minio/data"]`
- `services.minio.listenAddress = ":9000"` (all interfaces; firewall restricts)
- `networking.firewall.interfaces.tailscale0.allowedTCPPorts = [9000]`
- Firewall does NOT add 9000 to global `allowedTCPPorts` — port only reachable via tailscale0
- Minio root credentials loaded from `EnvironmentFile` (sops/agenix compatible)
- One-time bucket bootstrap: `mc mb local/crabcc-indexes` via activation script

### cdn-pop.nix

NGINX config:
```nginx
proxy_cache_path /var/cache/nginx/cdn
    levels=1:2 keys_zone=cdn:64m max_size=50g
    inactive=7d use_temp_path=off;

server {
    listen 8443 ssl;                          # 443 on ccx33
    server_name cdn.crabcc.app;
    ssl_certificate     /var/lib/acme/cdn.crabcc.app/fullchain.pem;
    ssl_certificate_key /var/lib/acme/cdn.crabcc.app/key.pem;

    slice 1m;
    proxy_cache cdn;
    proxy_cache_key "$uri$slice_range";
    proxy_set_header Range $slice_range;
    proxy_cache_valid 200 206 7d;
    proxy_cache_use_stale error timeout updating;

    add_header X-Cache-Status $upstream_cache_status;

    location / {
        proxy_pass http://100.105.72.88:9000;
    }
}
```

TLS: `security.acme.certs."cdn.crabcc.app"` with Cloudflare DNS-01 provider.
Cert dir readable by `nginx` group.

### cdn-traefik-patch.nix (dev-cx53 only)

Adds to `services.traefik.dynamicConfigOptions`:
```nix
tcp.routers."cdn-passthrough" = {
  rule       = "HostSNI(`cdn.crabcc.app`)";
  entryPoints = ["websecure"];
  service    = "cdn-nginx";
  tls.passthrough = true;
};
tcp.services."cdn-nginx".loadBalancer.servers = [
  { address = "127.0.0.1:8443"; }
];
```

### cdn-invalidator.nix

Systemd unit (`cdn-invalidator.service`):
- Runs `nats sub 'index.invalidated.>'` against `natsUrl`
- On message: extracts org/repo/branch from subject
- Calls `curl -s -X PURGE http://localhost:${nginxPort}/cache/purge?path=/<org>/<repo>/<branch>/*`
- Requires `ngx_cache_purge` module in NGINX (included via `services.nginx.package = pkgs.nginxMainline`)
- Restart: `on-failure`, `RestartSec = "5s"`

---

## Colmena Hive (`colmena.nix`)

```nix
{
  meta.nixpkgs = import <nixpkgs> {};

  dev-cx53 = { ... }: {
    imports = [ ./modules/cdn-origin.nix ./modules/cdn-pop.nix ./modules/cdn-traefik-patch.nix ./modules/cdn-invalidator.nix ];
    services.crabcc-cdn = {
      enable = true; domain = "cdn.crabcc.app";
      originUrl = "http://127.0.0.1:9000";  # local MinIO
      nginxPort = 8443;
      cloudflareTokenFile = /run/secrets/cloudflare-api-token;
      natsUrl = "nats://100.73.72.35:4222";
    };
    deployment.targetHost = "100.105.72.88";
    deployment.tags = ["cdn-pop" "cdn-origin"];
  };

  ccx33-nbg1-1 = { ... }: {
    imports = [ ./modules/cdn-pop.nix ./modules/cdn-invalidator.nix ];
    services.crabcc-cdn = {
      enable = true; domain = "cdn.crabcc.app";
      originUrl = "http://100.105.72.88:9000";  # dev-cx53 MinIO via tailscale
      nginxPort = 443;
      cloudflareTokenFile = /run/secrets/cloudflare-api-token;
      natsUrl = "nats://100.73.72.35:4222";
    };
    deployment.targetHost = "100.95.56.108";
    deployment.tags = ["cdn-pop"];
  };
}
```

---

## Security

| Threat | Mitigation |
|---|---|
| Direct MinIO access from internet | firewall: port 9000 closed on public NIC, open on tailscale0 only |
| PoP-to-origin without Tailscale | MinIO only reachable at 100.105.72.88:9000 (Tailscale IP) |
| Cert exposure | ACME cert stored in /var/lib/acme/, nginx group only |
| Unauthorized cache purge | purge endpoint bound to localhost only |
| NATS eviction spoofing | Phase 2 — add NATS auth; Phase 1 trusts tailnet ACL |

---

## Tailscale ACL prerequisite (applied 2026-06-11)

- `tag:proxy-node` → `tag:server` tcp:9000+4222 — ✅ applied
- dev-cx53 tagged `proxy-node` — ✅ applied
- Dead node `crabcc-ccx33-nbg1` deleted — ✅ done
- `tag:exit` removed from dev-cx53 — ✅ done

---

## Out of scope (Phase 2)

- Cloudflare Worker HMAC token validation
- Authenticated Origin Pull (Cloudflare client cert on NGINX)
- NATS credentials/auth (Phase 1 trusts tailnet)
- Multi-region PoPs (Phase 1 is nbg1 only)
- crabcc CLI `upload` + `cdn-url` subcommands

---

## Verification

```bash
# 1. MinIO accessible over tailnet
mc alias set cdn http://100.105.72.88:9000 $MINIO_USER $MINIO_PASS
mc ls cdn/crabcc-indexes

# 2. Cache slice working
curl -I -H "Range: bytes=0-1048575" https://cdn.crabcc.app/<path>
# Expect: X-Cache-Status: MISS first time, HIT on repeat

# 3. Invalidation working
nats pub index.invalidated.org.repo.main "" --server nats://100.73.72.35:4222
# Expect: cdn-invalidator logs purge on both PoPs
```
