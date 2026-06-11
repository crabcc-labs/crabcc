{
  description = "crabcc dev environment — Rust toolchain + the .tools CLI fleet + agent tooling (rtk) from llm-agents.nix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    llm-agents.url = "github:numtide/llm-agents.nix";
    # crane: Rust package builder with incremental dep caching
    crane.url = "github:ipetkov/crane";
    # fenix: nightly Rust toolchains (matches rust-toolchain.toml channel = "nightly")
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      llm-agents,
      crane,
      fenix,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;

        # Agent tooling from llm-agents.nix. `or { }` keeps the flake evaluating
        # on a system the upstream flake doesn't build for; the `?` guards below
        # only pull a package when it's actually exposed.
        agents = llm-agents.packages.${system} or { };

        # The .tools fleet, sourced from nixpkgs (a few attrs differ from the
        # binary name: task -> go-task, yq -> yq-go).
        toolFleet = with pkgs; [
          ripgrep
          fd
          bat
          eza
          delta
          sd
          choose
          dust
          procs
          bottom
          zoxide
          fzf
          jq
          yq-go
          xh
          hyperfine
          tokei
          hexyl
          watchexec
          ast-grep
          tree-sitter
          qsv
          dasel
          gh
          lazygit
          go-task
          sccache
          bun
          uv
        ];

        # Rust toolchain (from nixpkgs-unstable; track the crate MSRV in
        # Cargo.toml — bump the channel if a dep outruns it).
        rust = with pkgs; [
          rustc
          cargo
          clippy
          rustfmt
          rust-analyzer
        ];

        # Native build deps: clang + mold for the workspace's configured linker;
        # cmake + pkg-config for C-backed crates (e.g. aws-lc-rs via reqwest).
        buildDeps = with pkgs; [
          clang
          mold
          cmake
          pkg-config
        ];

        # rtk is crabcc's optional rewrite-chain stage (declared in .tools);
        # pull it from llm-agents.nix when available.
        agentTools = lib.optional (agents ? rtk) agents.rtk;

        # ── wormhole packages (crane + fenix nightly) ──────────────────────────
        # Use the latest nightly — matches `channel = "nightly"` in rust-toolchain.toml.
        # For a reproducible pin, replace with fenix.packages.${system}.fromToolchainFile.
        nightlyToolchain = fenix.packages.${system}.complete.toolchain;
        craneLib = (crane.mkLib pkgs).overrideToolchain nightlyToolchain;

        wormholeSrc = craneLib.cleanCargoSource ./.;

        wormholeArgs = {
          src = wormholeSrc;
          strictDeps = true;
          buildInputs = with pkgs;
            [ openssl ]
            ++ lib.optionals stdenv.isDarwin [
              darwin.apple_sdk.frameworks.Security
              darwin.apple_sdk.frameworks.SystemConfiguration
            ];
          nativeBuildInputs = with pkgs; [ pkg-config ];
        };

        # Build shared workspace deps once; reused by relay + node builds.
        wormholeDeps = craneLib.buildDepsOnly wormholeArgs;

        wormhole-relay = craneLib.buildPackage (wormholeArgs // {
          cargoArtifacts = wormholeDeps;
          cargoExtraArgs = "-p wormhole-relay --bin wormhole-relay";
          meta.mainProgram = "wormhole-relay";
        });

        wormhole-node = craneLib.buildPackage (wormholeArgs // {
          cargoArtifacts = wormholeDeps;
          cargoExtraArgs = "-p wormhole-node --bin wormhole-node";
          meta.mainProgram = "wormhole-node";
        });
      in
      {
        # ── packages ────────────────────────────────────────────────────────────
        packages = {
          inherit wormhole-relay wormhole-node;
          default = wormhole-node;
        };

        # ── CI checks ───────────────────────────────────────────────────────────
        checks = {
          inherit wormhole-relay wormhole-node;
          wormhole-clippy = craneLib.cargoClippy (wormholeArgs // {
            cargoArtifacts = wormholeDeps;
            cargoClippyExtraArgs = "-p wormhole-relay -p wormhole-node -- -D warnings";
          });
          wormhole-fmt = craneLib.cargoFmt { src = wormholeSrc; };
        };

        # ── dev shell (unchanged) ────────────────────────────────────────────────
        devShells.default = pkgs.mkShell {
          packages = rust ++ buildDeps ++ toolFleet ++ agentTools;
          shellHook = ''
            echo "crabcc devShell: rust $(rustc --version 2>/dev/null | cut -d' ' -f2) + the .tools fleet."
            echo "Build/test: task   |   fast build: task build-fast"
          '';
        };
      }
    )
    // {
      # ── NixOS modules — import in your flake's nixosConfigurations ────────────
      # Example:
      #   inputs.crabcc.url = "github:crabcc-labs/crabcc";
      #   modules = [ inputs.crabcc.nixosModules.wormhole-relay ];
      #   services.wormhole-relay = { enable = true; package = inputs.crabcc.packages.${system}.wormhole-relay; };
      nixosModules = {
        wormhole-relay = import ./nix/wormhole-relay.nix;
        wormhole-node = import ./nix/wormhole-node.nix;
      };

      # Overlay to inject wormhole-relay + wormhole-node into nixpkgs
      overlays.default = final: prev: {
        wormhole-relay = self.packages.${prev.system}.wormhole-relay;
        wormhole-node = self.packages.${prev.system}.wormhole-node;
      };
    };
}
