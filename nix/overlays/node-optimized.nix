# node-optimized: Node.js 26 with:
#   - aws-lc 1.69.0 replacing OpenSSL — hardware AES-GCM / ChaCha20 / ECDH on aarch64
#   - ThinLTO across V8, libuv, and all C++ bindings
#   - jemalloc — reduces allocator fragmentation under V8's GC pressure
#   - small-icu — English-only ICU data; saves ~15 MB and speeds cold start
#
# Bump nodejs_28 when it lands in nixpkgs-unstable.
# To update aws-lc: nix eval nixpkgs#aws-lc.version --raw
final: prev:
let
  base = (prev.nodejs_26 or prev.nodejs_24).override {
    # aws-lc is API/ABI-compatible with OpenSSL for the Node.js bindings.
    # On aarch64 it uses ARMv8 crypto extensions for AES-GCM and SHA.
    openssl = prev.aws-lc;
  };
in {
  node-optimized = base.overrideAttrs (old: {
    pname = "node-optimized";

    # jemalloc linked into the node binary; reduces fragmentation in long-lived
    # processes with V8's frequent small allocations.
    buildInputs = (old.buildInputs or [ ]) ++ [ prev.jemalloc ];

    configureFlags = old.configureFlags ++ [
      "--with-jemalloc"
      "--with-intl=small-icu" # replaces --with-intl=system-icu; English only
    ];

    env = (old.env or { }) // {
      NIX_CFLAGS_COMPILE =
        ((old.env or { }).NIX_CFLAGS_COMPILE or "") + " -flto=thin";
      NIX_LDFLAGS =
        ((old.env or { }).NIX_LDFLAGS or "") + " -flto=thin";
    };
  });
}
