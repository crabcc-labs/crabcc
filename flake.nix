{
  description = "crabcc dev environment — Rust toolchain + the .tools CLI fleet + agent tooling (rtk) from llm-agents.nix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    llm-agents.url = "github:numtide/llm-agents.nix";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      llm-agents,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };

        # Agent tooling from llm-agents.nix. `or { }` keeps the flake evaluating
        # on a system the upstream flake doesn't build for; the `?` guards below
        # only pull a package when it's actually exposed.
        agents = llm-agents.packages.${system} or { };

        # The .tools fleet, sourced from nixpkgs (a few attrs differ from the
        # binary name: dust -> du-dust, task -> go-task, yq -> yq-go).
        toolFleet = with pkgs; [
          ripgrep
          fd
          bat
          eza
          delta
          sd
          choose
          du-dust
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
        agentTools = pkgs.lib.optional (agents ? rtk) agents.rtk;
      in
      {
        devShells.default = pkgs.mkShell {
          packages = rust ++ buildDeps ++ toolFleet ++ agentTools;
          shellHook = ''
            echo "crabcc devShell: rust $(rustc --version 2>/dev/null | cut -d' ' -f2) + the .tools fleet."
            echo "Build/test: task   |   fast build: task build-fast"
          '';
        };
      }
    );
}
