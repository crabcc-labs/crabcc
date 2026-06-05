# Adds node-optimized to nixpkgs: Node.js 24 (latest) compiled with ThinLTO.
#
# V8 handles JS optimization at runtime (Sparkplug -> Maglev -> Turbofan);
# ThinLTO here targets the C++ layer: V8 internals, libuv, OpenSSL bindings.
#
# Bump the base attribute when nixpkgs-unstable ships a newer LTS:
#   base = prev.nodejs_28 or prev.nodejs_26 or prev.nodejs;
final: prev: {
  node-optimized = (prev.nodejs_26 or prev.nodejs_24).overrideAttrs (old: {
    pname = "node-optimized";
    env = (old.env or { }) // {
      NIX_CFLAGS_COMPILE =
        ((old.env or { }).NIX_CFLAGS_COMPILE or "") + " -flto=thin";
      NIX_LDFLAGS =
        ((old.env or { }).NIX_LDFLAGS or "") + " -flto=thin";
    };
  });
}
