# Adds python315t-optimized to nixpkgs:
#   - CPython 3.15 pre-release (free-threaded / no-GIL)
#   - PGO: multistage instrument -> profile -> recompile (--enable-optimizations)
#   - ThinLTO (--with-lto=thin)
#   - Copy-and-patch JIT (--enable-experimental-jit)
#
# Update rev + hash when a new pre-release tag drops (e.g. 3.15.0b3, 3.15.0rc1):
#   nix-prefetch-url --unpack https://github.com/python/cpython/archive/refs/tags/vX.Y.tar.gz
#   nix hash convert --to sri sha256:<base32-output>
final: prev:
let
  cpython315 = {
    rev  = "v3.15.0b2";
    hash = "sha256-ILvnNPQpm8ACvWFigmFfrAcAGXdXXrHEMgh/dZ4QMcQ=";
  };

  # Ride on the free-threaded 3.13 derivation (closest upstream base).
  # python315t won't be in nixpkgs until 3.15 stabilises; we build it here.
  base = prev.python313t or prev.python313;
in {
  python315t-optimized = (base.override {
    enableOptimizations = true; # PGO multistage build (non-deterministic profile data)
    reproducibleBuild   = false;
  }).overrideAttrs (old: {
    pname   = "python315t-optimized";
    version = "3.15.0b2";

    src = prev.fetchFromGitHub {
      owner = "python";
      repo  = "cpython";
      inherit (cpython315) rev hash;
    };

    # Append our flags; the base free-threaded derivation already sets
    # --enable-free-threading, so we only add what's missing.
    configureFlags = (old.configureFlags or []) ++ [
      "--enable-free-threading" # idempotent: ensures no-GIL regardless of base
      "--enable-experimental-jit" # copy-and-patch specializing bytecode JIT
      "--with-lto=thin"           # ThinLTO: fast incremental link + good codegen
    ];
  });
}
