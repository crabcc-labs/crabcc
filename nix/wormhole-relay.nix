{ config, lib, pkgs, ... }:
let
  cfg = config.services.wormhole-relay;
in {
  options.services.wormhole-relay = {
    enable = lib.mkEnableOption "wormhole-relay — crabcc Noise_IK relay";

    port = lib.mkOption {
      type = lib.types.port;
      default = 4443;
      description = "WebSocket listen port (WORMHOLE_PORT).";
    };

    relayTokenFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Path to a file containing the bearer token for the /replay endpoint.
        When null, the replay endpoint is unauthenticated (development only).
      '';
    };

    package = lib.mkOption {
      type = lib.types.package;
      description = "The wormhole-relay package to use.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.wormhole-relay = {
      description = "crabcc wormhole-relay";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      environment = {
        WORMHOLE_PORT = toString cfg.port;
      };

      serviceConfig = {
        Type = "simple";
        ExecStart = "${cfg.package}/bin/wormhole-relay";

        # Relay keeps no persistent state — DynamicUser gives it a transient UID.
        DynamicUser = true;

        # Hardening
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        CapabilityBoundingSet = "";
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" ];
        SystemCallFilter = [ "@system-service" "~@privileged" ];

        Restart = "on-failure";
        RestartSec = "2s";
      };

      # Inject relay token from file if configured
      preStart = lib.mkIf (cfg.relayTokenFile != null) ''
        export RELAY_TOKEN=$(cat ${cfg.relayTokenFile})
      '';
    };

    networking.firewall.allowedTCPPorts = [ cfg.port ];
  };
}
