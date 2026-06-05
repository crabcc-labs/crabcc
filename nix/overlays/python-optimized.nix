# Adds python315t-optimized to nixpkgs:
#   - CPython 3.15 pre-release (free-threaded / no-GIL)
#   - PGO: multistage instrument -> profile -> recompile (--enable-optimizations)
#   - FullLTO (--with-lto=full)
#   - Copy-and-patch JIT (--enable-experimental-jit)
#   - aws-lc for the ssl module (ARMv8 hardware crypto)
#   - mimalloc allocator (lower fragmentation under free-threaded GC)
#   - -fno-semantic-interposition (devirtualisation across TUs)
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
    openssl             = prev.aws-lc; # ARMv8 hardware AES-GCM/ChaCha20/ECDH
  }).overrideAttrs (old: {
    pname   = "python315t-optimized";
    version = "3.15.0b2";

    src = prev.fetchFromGitHub {
      owner = "python";
      repo  = "cpython";
      inherit (cpython315) rev hash;
    };

    buildInputs = (old.buildInputs or []) ++ [
      prev.mimalloc # --with-mimalloc: lower fragmentation under free-threaded GC
    ];

    # Append our flags; the base free-threaded derivation already sets
    # --enable-free-threading, so we only add what's missing.
    configureFlags = (old.configureFlags or []) ++ [
      "--enable-free-threading"  # idempotent: ensures no-GIL regardless of base
      "--enable-experimental-jit" # copy-and-patch specializing bytecode JIT
      "--with-lto=full"           # full LTO: maximum cross-TU inlining
      "--with-mimalloc"           # use mimalloc as the object allocator
    ];

    env = (old.env or {}) // {
      NIX_CFLAGS_COMPILE =
        ((old.env or {}).NIX_CFLAGS_COMPILE or "")
        + " -fno-semantic-interposition"; # allows devirt across shared-lib boundaries
    };
  });
}
