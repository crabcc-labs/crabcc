# Adds python316t-optimized to nixpkgs:
#   - CPython 3.16 pre-release (free-threaded / no-GIL)
#   - PGO: multistage instrument -> profile -> recompile (--enable-optimizations)
#   - ThinLTO (--with-lto=thin)
#   - Copy-and-patch JIT (--enable-experimental-jit; may become --enable-jit in 3.16 final)
#
# Update rev + hash when a new 3.16 pre-release tag drops:
#   nix-prefetch-github --owner python --repo cpython --rev v3.16.0aN
#   or: nix store prefetch-file --hash-type sha256 \
#         https://github.com/python/cpython/archive/refs/tags/v3.16.0aN.tar.gz
final: prev:
let
  cpython316 = {
    rev  = "v3.16.0a1"; # bump as new pre-release tags appear
    hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="; # FILL IN after nix-prefetch
  };

  # Ride on the free-threaded 3.13 derivation (closest upstream base).
  # python316t won't exist in nixpkgs until it stabilises; we build it here.
  base = prev.python313t or prev.python313;
in {
  python316t-optimized = (base.override {
    enableOptimizations = true; # PGO multistage build (non-deterministic profile data)
    reproducibleBuild   = false;
  }).overrideAttrs (old: {
    pname   = "python316t-optimized";
    version = "3.16.0a1";

    src = prev.fetchFromGitHub {
      owner = "python";
      repo  = "cpython";
      inherit (cpython316) rev hash;
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
