# node-optimized: Node.js 26 with every perf knob turned on.
#
# Stack:
#   aws-lc      — ARMv8 crypto extensions for AES-GCM/ChaCha20/ECDH
#   jemalloc    — lower fragmentation under V8's small-alloc GC pressure
#   small-icu   — English-only ICU data, saves ~15 MB, faster cold start
#   mold        — parallel linker; ThinLTO-aware via LLVM plugin
#   ThinLTO     — cross-TU inlining for V8/libuv/bindings C++ layer
#   -mcpu=apple-m3 — M3-tuned codegen (fused ops, crypto ext, SVE2 hints)
#   hugepages   — 2 MB pages for V8 code space, reduces JIT TLB pressure
#   ptr-compress — V8 heap pointers 64→32 bit; ~30% heap savings (4 GB cap)
#   semi-space  — 1 GB per semi-space (2 GB young gen); reduces premature
#                 promotion and Old-Space GC frequency on high-alloc workloads.
#                 Baked via NODE_OPTIONS; override at runtime if needed.
#   shared deps — libuv, zlib, c-ares, nghttp2, brotli; smaller binary, OS-managed updates
#   PGO         — NOTE: not applied here. Node.js PGO requires two separate
#                 configure+build phases (--enable-pgo-generate then
#                 --enable-pgo-use). That needs a custom buildPhase and a
#                 training workload run between phases. Add when needed.
#
# mold note: wild is not yet in nixpkgs; revisit when it lands.
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

    nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
      # mold-wrapped registers itself as `ld` in the Nix cc-wrapper;
      # -fuse-ld=mold below makes GYP/cmake use it explicitly.
      prev.mold-wrapped
      # makeWrapper: used in postInstall to bake NODE_OPTIONS into the binary.
      prev.makeWrapper
    ];

    buildInputs = (old.buildInputs or [ ]) ++ [
      prev.jemalloc  # --with-jemalloc
      prev.libuv     # --shared-libuv
      prev.zlib      # --shared-zlib
      prev.c-ares    # --shared-cares   (async DNS resolution)
      prev.nghttp2   # --shared-nghttp2 (HTTP/2)
      prev.brotli    # --shared-brotli  (Brotli compression)
    ];

    configureFlags =
      # Drop the nixpkgs default --with-intl=system-icu; we replace it below.
      (lib.filter (f: !(lib.hasPrefix "--with-intl=" f)) old.configureFlags)
      ++ [
        "--with-jemalloc"
        "--with-intl=small-icu"
        "--shared-libuv"
        "--shared-zlib"
        "--shared-cares"
        "--shared-nghttp2"
        "--shared-brotli"

        # V8 tweaks
        "--v8-enable-hugepage"
        # Pointer compression: V8 heap pointers 64→32 bit offsets within a
        # 4 GB cage. ~30% heap savings; max heap hard-capped at 4 GB.
        # Flag name: verify if Node.js 26 graduated this from --experimental-*.
        "--experimental-enable-pointer-compression"

        # PGO would go here as --enable-pgo-generate / --enable-pgo-use but
        # requires a custom two-phase buildPhase. Omitted for now.
      ];

    postInstall = (old.postInstall or "") + ''
      wrapProgram $out/bin/node \
        --set-default NODE_OPTIONS "--max-semi-space-size=1024"
    '';

    env = (old.env or { }) // {
      # ThinLTO + M3-specific codegen. Both passes (PGO instrument + final)
      # use these flags — clang handles the combined PGO+LTO pipeline.
      # -fuse-ld=mold: clang driver flag that selects mold at link time.
      # Must appear in compile flags so it propagates to the link step;
      # mold-wrapped in nativeBuildInputs ensures mold is in PATH.
      NIX_CFLAGS_COMPILE =
        ((old.env or { }).NIX_CFLAGS_COMPILE or "")
        + " -flto=thin -mcpu=apple-m3 -fuse-ld=mold";
      NIX_LDFLAGS =
        ((old.env or { }).NIX_LDFLAGS or "") + " -flto=thin";
    };
  });
}
