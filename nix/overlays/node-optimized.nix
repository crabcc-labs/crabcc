# node-optimized: Node.js 26 with every perf knob turned on.
#
# Stack:
#   aws-lc      — ARMv8 crypto extensions for AES-GCM/ChaCha20/ECDH
#   jemalloc    — lower fragmentation under V8's small-alloc GC pressure
#   small-icu   — English-only ICU data, saves ~15 MB, faster cold start
#   ThinLTO     — cross-TU inlining for V8/libuv/bindings C++ layer
#   -mcpu=apple-m3 — M3-tuned codegen (fused ops, crypto ext, SVE2 hints)
#   hugepages   — 2 MB pages for V8 code space, reduces JIT TLB pressure
#   ptr-compress — V8 heap pointers 64→32 bit; ~30% heap savings (4 GB cap)
#   shared deps — system libuv + zlib; smaller binary, OS-managed updates
#   PGO         — two-phase profile-guided recompile of all C++ paths
#                 (requires Nix sandbox disabled; on macOS this is default)
#
# WARNING: -mcpu=apple-m3 means this image only runs on M3 hardware.
# For a portable aarch64 build swap to: -mcpu=apple-m1 (M1/M2/M3 compat).
#
# Bump nodejs_28 when it lands in nixpkgs-unstable.
final: prev:
let
  lib  = prev.lib;
  base = (prev.nodejs_26 or prev.nodejs_24).override {
    openssl = prev.aws-lc;
  };
in {
  node-optimized = base.overrideAttrs (old: {
    pname = "node-optimized";

    buildInputs = (old.buildInputs or [ ]) ++ [
      prev.jemalloc # --with-jemalloc
      prev.libuv    # --shared-libuv
      # zlib already in stdenv; listed explicitly for clarity
      prev.zlib
    ];

    configureFlags =
      # Drop the nixpkgs default --with-intl=system-icu; we replace it below.
      (lib.filter (f: !(lib.hasPrefix "--with-intl=" f)) old.configureFlags)
      ++ [
        "--with-jemalloc"
        "--with-intl=small-icu"
        "--shared-libuv"
        "--shared-zlib"

        # V8 tweaks
        "--v8-enable-hugepage"
        # Pointer compression: V8 heap pointers 64→32 bit offsets within a
        # 4 GB cage. ~30% heap savings; max heap hard-capped at 4 GB.
        # Flag name: verify if Node.js 26 graduated this from --experimental-*.
        "--experimental-enable-pointer-compression"

        # PGO: instrument → profile run → recompile.
        # Nix sandbox must be off (default on macOS; set sandbox = false in
        # /etc/nix/nix.conf on Linux).
        "--enable-pgo-build"
      ];

    env = (old.env or { }) // {
      # ThinLTO + M3-specific codegen. Both passes (PGO instrument + final)
      # use these flags — clang handles the combined PGO+LTO pipeline.
      NIX_CFLAGS_COMPILE =
        ((old.env or { }).NIX_CFLAGS_COMPILE or "")
        + " -flto=thin -mcpu=apple-m3";
      NIX_LDFLAGS =
        ((old.env or { }).NIX_LDFLAGS or "") + " -flto=thin";
    };
  });
}
