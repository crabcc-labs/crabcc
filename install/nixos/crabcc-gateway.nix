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
      };
    };

    # oauth2-proxy, traefik, and firewall wired in subsequent tasks
  };
}
