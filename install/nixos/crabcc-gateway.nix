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
        MemoryDenyWriteExecute = true;
        PrivateDevices = true;
        PrivateIPC = true;
      };
    };

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
      trustedProxyIP = [ "127.0.0.1/32" "::1/128" ];

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
    };

    # ── Firewall: only expose 80 (redirect) and 443 (MCP HTTPS) ────────────
    networking.firewall.allowedTCPPorts = [ 80 443 ];
  };
}
