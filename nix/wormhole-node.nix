{ config, lib, pkgs, ... }:
let
  cfg = config.services.wormhole-node;
in {
  options.services.wormhole-node = {
    enable = lib.mkEnableOption "wormhole-node — crabcc Noise_IK node daemon";

    relayUrl = lib.mkOption {
      type = lib.types.str;
      default = "ws://127.0.0.1:4443/wormhole/v1";
      description = "Relay WebSocket URL (WORMHOLE_RELAY_URL).";
    };

    keysFile = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/wormhole-node/node-keys.bin";
      description = "Path to the node static key file (auto-generated on first start).";
    };

    opStaticPubFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Path to a file containing the hex-encoded operator static public key.
        When set, the node pre-authorises connections from that operator key
        without requiring SPAKE2 pairing.
      '';
    };

    package = lib.mkOption {
      type = lib.types.package;
      description = "The wormhole-node package to use.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.wormhole-node = {
      description = "crabcc wormhole-node";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      environment = {
        WORMHOLE_RELAY_URL = cfg.relayUrl;
        WORMHOLE_KEYS_FILE = cfg.keysFile;
      };

      serviceConfig = {
        Type = "simple";
        ExecStart = "${cfg.package}/bin/wormhole-node";

        # Node needs a stable state dir for key persistence
        StateDirectory = "wormhole-node";
        StateDirectoryMode = "0700";
        User = "wormhole-node";
        Group = "wormhole-node";

        # Hardening
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ReadWritePaths = [ "/var/lib/wormhole-node" "/tmp" ];
        CapabilityBoundingSet = "";
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        SystemCallFilter = [ "@system-service" "~@privileged" ];

        Restart = "always";
        RestartSec = "3s";
      };

      preStart = lib.mkIf (cfg.opStaticPubFile != null) ''
        export WORMHOLE_OP_STATIC_PUB=$(cat ${cfg.opStaticPubFile})
      '';
    };

    users.users.wormhole-node = {
      isSystemUser = true;
      group = "wormhole-node";
      home = "/var/lib/wormhole-node";
    };
    users.groups.wormhole-node = {};
  };
}
