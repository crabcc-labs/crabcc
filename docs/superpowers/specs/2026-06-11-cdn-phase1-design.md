# cdn.crabcc.app Phase 1 — Design Spec

> Phase 1: Origin + dual PoP, no Cloudflare Worker yet. Direct HTTPS access to
> `cdn.crabcc.app` is the test surface. HMAC token validation and AOP certs come
> in Phase 2.

**Date:** 2026-06-11 (revised post team review)
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
[NGINX :8443 on dev-cx53 / :443 on ccx33]
  cert: Cloudflare DNS-01 ACME for cdn.crabcc.app
  cache: /var/cache/nginx/cdn  (slice 1m, NVMe-backed)
     │
     │ cache miss → proxy_pass via tailscale0
     ▼
[MinIO on dev-cx53 :9000]
  bind: 100.105.72.88:9000  (tailscale IP only — NOT 0.0.0.0)
  bucket policy: public-read (NGINX does unauthenticated proxy_pass)
     │
Invalidation (JetStream durable — PoP-offline-safe):
  Stream: CDN_INVALIDATE, subject: index.invalidated.<org>.<repo>.<branch>
  Consumer per PoP: pull, AckExplicit, DeliverAll
  On message: ngx_cache_purge matching cache keys → Ack
```

**Traefik TCP passthrough on dev-cx53:**
- SNI `cdn.crabcc.app` → TCP passthrough → NGINX :8443
- SNI `mcp.yourdomain.com` → existing Layer 7 Traefik handling

**ccx33-nbg1-1** has no Traefik — NGINX binds :443 directly.

---

## Fleet Structure

```
__cdn/nix/
  flake.nix              # inputs: nixpkgs (pinned nixos-25.05), colmena, nixos-anywhere
  colmena.nix            # hive: dev-cx53 (origin+pop) + ccx33-nbg1-1 (pop)
  modules/
    cdn-origin.nix       # services.minio (tailscale IP bind), bucket bootstrap unit
    cdn-pop.nix          # services.nginx, ACME, cache config, options
    cdn-traefik-patch.nix # Traefik tcp.router for SNI passthrough (dev-cx53 only)
    cdn-invalidator.nix  # systemd unit: JetStream pull consumer → ngx_cache_purge
  hosts/
    ccx33-nbg1-1.nix     # base NixOS config: hostname, SSH, Tailscale, disko layout
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
  # Path to file containing: CLOUDFLARE_DNS_API_TOKEN=<token>
  # Required Cloudflare permission: Zone:DNS:Edit on zone crabcc.app
  cloudflareEnvFile    = /run/secrets/cloudflare-acme-env;
  natsUrl              = "nats://100.73.72.35:4222";
  natsStream           = "CDN_INVALIDATE";
  natsBucket           = "crabcc-indexes";
};
```

Renamed `cloudflareTokenFile` → `cloudflareEnvFile` to match NixOS ACME option
(`security.acme.certs.<domain>.environmentFile`).

---

## Component Specs

### cdn-origin.nix (dev-cx53 only)

```nix
services.minio = {
  enable = true;
  dataDir = [ "/var/lib/minio/data" ];
  listenAddress = "100.105.72.88:9000";  # tailscale IP — NOT :9000 / 0.0.0.0
  # Root credentials from sops-nix secret:
  # file: /run/secrets/minio-env
  # format: MINIO_ROOT_USER=...\nMINIO_ROOT_PASSWORD=...
  environmentFile = "/run/secrets/minio-env";
};

networking.firewall.enable = true;
# Port 9000 is NOT in allowedTCPPorts (global) — only reachable via tailscale0 IP.
# MinIO bound to tailscale IP is the primary isolation; firewall is defense-in-depth.

# Bucket bootstrap: dedicated systemd oneshot, After = minio.service
systemd.services.minio-bucket-bootstrap = {
  description = "Create crabcc-indexes bucket in MinIO";
  after = [ "minio.service" ];
  wants = [ "minio.service" ];
  wantedBy = [ "multi-user.target" ];
  serviceConfig = {
    Type = "oneshot";
    RemainAfterExit = true;
    EnvironmentFile = "/run/secrets/minio-env";
    ExecStartPre = "${pkgs.bash}/bin/bash -c 'until ${pkgs.curl}/bin/curl -sf http://100.105.72.88:9000/minio/health/live; do sleep 2; done'";
    ExecStart = "${pkgs.minio-client}/bin/mc --no-color alias set local http://100.105.72.88:9000 $MINIO_ROOT_USER $MINIO_ROOT_PASSWORD && ${pkgs.minio-client}/bin/mc mb --ignore-existing local/crabcc-indexes && ${pkgs.minio-client}/bin/mc anonymous set download local/crabcc-indexes";
  };
};
```

The `mc anonymous set download` call sets a public-read policy on the bucket so
NGINX can `proxy_pass` without credentials.

### cdn-pop.nix

NGINX config:
```nix
services.nginx = {
  enable = true;
  # ngx_cache_purge is NOT in nginxMainline by default — must be explicit
  additionalModules = [ pkgs.nginxModules.cache-purge ];

  commonHttpConfig = ''
    proxy_cache_path ${cfg.cacheDir}
        levels=1:2 keys_zone=cdn:64m max_size=${cfg.cacheSize}
        inactive=7d use_temp_path=off;
  '';

  virtualHosts."${cfg.domain}" = {
    listen = [{ port = cfg.nginxPort; ssl = true; }];
    sslCertificate = "/var/lib/acme/${cfg.domain}/fullchain.pem";
    sslCertificateKey = "/var/lib/acme/${cfg.domain}/key.pem";

    extraConfig = ''
      slice 1m;
      proxy_cache cdn;
      proxy_cache_key "$host$uri$slice_range";
      proxy_set_header Range $slice_range;
      proxy_cache_valid 200 206 7d;
      proxy_cache_use_stale error timeout updating
                            http_500 http_502 http_503 http_504;
      proxy_cache_lock on;
      add_header X-Cache-Status $upstream_cache_status;

      location / {
          proxy_pass ${cfg.originUrl};
      }

      # ngx_cache_purge endpoint — localhost only
      location ~ /purge(/.*) {
          allow 127.0.0.1;
          deny all;
          proxy_cache_purge cdn "$host$1";
      }
    '';
  };
};

# ACME: DNS-01 via Cloudflare — no port 80 dependency
security.acme = {
  acceptTerms = true;
  certs."${cfg.domain}" = {
    dnsProvider = "cloudflare";
    environmentFile = cfg.cloudflareEnvFile;
    # nginx group must read the cert
    group = "nginx";
  };
};
```

### cdn-traefik-patch.nix (dev-cx53 only)

Extends `services.traefik.dynamicConfigOptions`:
```nix
services.traefik.dynamicConfigOptions = lib.mkMerge [
  config.services.traefik.dynamicConfigOptions
  {
    tcp.routers."cdn-passthrough" = {
      rule        = "HostSNI(`${cfg.domain}`)";
      entryPoints = [ "websecure" ];
      service     = "cdn-nginx";
      tls.passthrough = true;
    };
    tcp.services."cdn-nginx".loadBalancer.servers = [
      { address = "127.0.0.1:8443"; }
    ];
  }
];
```

Note: Traefik's `websecure` entrypoint handles both HTTP (Layer 7) and TCP (Layer 4)
routes simultaneously via SNI inspection. No entrypoint config change needed.

### cdn-invalidator.nix

JetStream pull consumer (durable delivery — messages survive PoP restarts):
```nix
systemd.services.cdn-invalidator = {
  description = "CDN cache invalidator — NATS JetStream → ngx_cache_purge";
  after = [ "network.target" "tailscaled.service" ];
  wantedBy = [ "multi-user.target" ];
  path = [ pkgs.natscli pkgs.curl ];
  script = ''
    set -euo pipefail
    # Durable consumer: DeliverAll, AckExplicit
    # If consumer already exists, nats consumer add is idempotent
    nats --server "${cfg.natsUrl}" consumer add "${cfg.natsStream}" \
      "cdn-pop-$(hostname)" --deliver all --ack explicit --pull \
      --filter "index.invalidated.>" 2>/dev/null || true

    nats --server "${cfg.natsUrl}" consumer next \
      "${cfg.natsStream}" "cdn-pop-$(hostname)" --count 0 |
    while IFS= read -r subject; do
      # subject: index.invalidated.<org>.<repo>.<branch>
      path=$(echo "$subject" | sed 's|index\.invalidated\.|/|; s|\.|/|g')
      curl -sf -X PURGE "http://127.0.0.1:${toString cfg.nginxPort}/purge$path" || true
    done
  '';
  serviceConfig = {
    Restart = "on-failure";
    RestartSec = "5s";
    NoNewPrivileges = true;
  };
};
```

---

## Colmena Hive (`colmena.nix`)

```nix
# flake.nix wires: inputs.nixpkgs.follows = "nixpkgs"; meta.nixpkgs uses flake input
{
  meta.nixpkgs = import inputs.nixpkgs { system = "x86_64-linux"; };

  dev-cx53 = { ... }: {
    imports = [
      ./modules/cdn-origin.nix
      ./modules/cdn-pop.nix
      ./modules/cdn-traefik-patch.nix
      ./modules/cdn-invalidator.nix
    ];
    services.crabcc-cdn = {
      enable = true; domain = "cdn.crabcc.app";
      originUrl = "http://127.0.0.1:9000";  # local MinIO — no tailscale hairpin
      nginxPort = 8443;
      cloudflareEnvFile = /run/secrets/cloudflare-acme-env;
      natsUrl = "nats://100.73.72.35:4222";
    };
    deployment.targetHost = "100.105.72.88";
    deployment.tags = ["cdn-pop" "cdn-origin"];
  };

  ccx33-nbg1-1 = { ... }: {
    imports = [
      ./hosts/ccx33-nbg1-1.nix    # base: hostname, SSH, Tailscale, disko
      ./modules/cdn-pop.nix
      ./modules/cdn-invalidator.nix
    ];
    services.crabcc-cdn = {
      enable = true; domain = "cdn.crabcc.app";
      originUrl = "http://100.105.72.88:9000";  # dev-cx53 via tailscale
      nginxPort = 443;
      cloudflareEnvFile = /run/secrets/cloudflare-acme-env;
      natsUrl = "nats://100.73.72.35:4222";
    };
    deployment.targetHost = "100.95.56.108";
    deployment.tags = ["cdn-pop"];
  };
}
```

**Apply order (important):** always `colmena apply --on dev-cx53` first, then
`colmena apply --on ccx33-nbg1-1`. This ensures MinIO is running before ccx33
makes cache-miss origin requests.

---

## ccx33-nbg1-1 NixOS Baseline (`hosts/ccx33-nbg1-1.nix`)

```nix
{ modulesPath, ... }: {
  imports = [ (modulesPath + "/profiles/qemu-guest.nix") ];

  networking.hostName = "ccx33-nbg1-1";

  # Disk layout via disko (nixos-anywhere requires this)
  # CCX33: /dev/sda is the root disk (225 GB)
  disko.devices.disk.sda = {
    type = "disk"; device = "/dev/sda";
    content = {
      type = "gpt";
      partitions = {
        boot = { size = "1M"; type = "EF02"; };  # GRUB BIOS
        root = {
          size = "100%";
          content = { type = "filesystem"; format = "ext4"; mountpoint = "/"; };
        };
      };
    };
  };

  # SSH: deploy key (same as dev-cx53 for Colmena)
  users.users.root.openssh.authorizedKeys.keys = [ "<deploy-pubkey>" ];

  # Tailscale: join tailnet with pre-auth key on first boot
  services.tailscale.enable = true;
  systemd.services.tailscale-autoconnect = {
    after = [ "tailscaled.service" ]; wantedBy = [ "multi-user.target" ];
    serviceConfig.Type = "oneshot";
    script = ''
      ${pkgs.tailscale}/bin/tailscale up \
        --authkey "$(cat /run/secrets/tailscale-authkey)" \
        --advertise-tags "tag:server,tag:proxy-node" \
        --hostname ccx33-nbg1-1
    '';
  };

  networking.firewall.enable = true;
  networking.firewall.allowedTCPPorts = [ 443 22 ];
}
```

---

## nixos-anywhere Conversion Procedure

```bash
# 1. Pre-conversion (do these BEFORE running nixos-anywhere)
#    a. Request Hetzner KVM console access for ccx33-nbg1-1 (fallback if boot fails)
#    b. Generate Tailscale pre-auth key (one-time, tagged tag:server,tag:proxy-node)
#       → save as /run/secrets/tailscale-authkey on your Mac
#    c. Ensure cloudflare-acme-env and minio-env secrets are encrypted with ccx33's age key
#       (sops-nix: add ccx33 host key to .sops.yaml and re-encrypt)
#    d. Confirm ccx33 is not serving production traffic (it's currently bare Ubuntu)

# 2. Run nixos-anywhere
nix run github:nix-community/nixos-anywhere -- \
  --flake .#ccx33-nbg1-1 \
  --target-host root@100.95.56.108 \
  --extra-files /tmp/secrets-for-boot   # inject tailscale-authkey pre-boot

# 3. Verify NixOS booted
ssh root@100.95.56.108 'nixos-version && tailscale status'

# 4. If boot fails: attach Hetzner KVM, boot rescue, restore Ubuntu from snapshot or
#    re-run nixos-anywhere after fixing the NixOS config.
```

---

## Secrets (sops-nix)

| Secret path | Format | Used by | Both nodes? |
|---|---|---|---|
| `/run/secrets/minio-env` | `MINIO_ROOT_USER=...\nMINIO_ROOT_PASSWORD=...` | cdn-origin.nix, minio-bucket-bootstrap | dev-cx53 only |
| `/run/secrets/cloudflare-acme-env` | `CLOUDFLARE_DNS_API_TOKEN=...` | cdn-pop.nix (ACME) | both |
| `/run/secrets/tailscale-authkey` | plain text | ccx33 first boot | ccx33 only |

Scope: `CLOUDFLARE_DNS_API_TOKEN` requires `Zone:DNS:Edit` on zone `crabcc.app`.

---

## Security

| Threat | Mitigation |
|---|---|
| MinIO access from internet | Bound to tailscale IP 100.105.72.88:9000 — unreachable without tailnet auth |
| Unauthorized object read | Bucket is public-read; objects are content-addressed by hash — no sensitive data |
| Direct MinIO write/delete | MinIO creds not exposed to PoPs; NGINX does GET-only proxy_pass |
| Unauthorized cache purge | `/purge` location: `allow 127.0.0.1; deny all` |
| NATS invalidation flood DoS | Accepted risk for Phase 1; any proxy-node compromise can flood invalidations. Mitigated in Phase 2 with NATS credentials. |
| PoP-to-origin without tailnet | MinIO only on tailscale IP; tailnet ACL enforces tag:proxy-node scope |

---

## Verification

```bash
# 1. MinIO accessible over tailnet + bucket exists
mc alias set cdn http://100.105.72.88:9000 $MINIO_ROOT_USER $MINIO_ROOT_PASSWORD
mc ls cdn/crabcc-indexes

# 2. MinIO port NOT reachable from public internet
nc -zv 46.225.127.20 9000   # dev-cx53 public IP — should time out

# 3. TLS cert issued and valid on both PoPs
openssl s_client -connect cdn.crabcc.app:443 -servername cdn.crabcc.app \
  </dev/null | openssl x509 -noout -dates

# 4. Traefik SNI passthrough working on dev-cx53
curl -sI --resolve cdn.crabcc.app:443:100.105.72.88 https://cdn.crabcc.app/ \
  | grep x-cache-status   # should not return Traefik headers

# 5. Cache slice working (MISS then HIT)
curl -I -H "Range: bytes=0-1048575" https://cdn.crabcc.app/<path>  # X-Cache-Status: MISS
curl -I -H "Range: bytes=0-1048575" https://cdn.crabcc.app/<path>  # X-Cache-Status: HIT

# 6. Invalidation end-to-end (cache actually purged, not just logged)
nats pub index.invalidated.org.repo.main "" --server nats://100.73.72.35:4222
curl -I -H "Range: bytes=0-1048575" https://cdn.crabcc.app/<path>  # should be MISS again
```

---

## Out of scope (Phase 2)

- Cloudflare Worker HMAC token validation
- Authenticated Origin Pull (Cloudflare client cert on NGINX)
- NATS credentials/auth (Phase 1 trusts tailnet ACL; flood-DoS risk accepted)
- Multi-region PoPs (Phase 1 is nbg1 only)
- crabcc CLI `upload` + `cdn-url` subcommands
