# Entry point for Nixery.
# Set NIXERY_PKGS_PATH to the directory containing this file.
# Nixery calls: import <NIXERY_PKGS_PATH> {}
#
# Uses the system's <nixpkgs> channel.  Point the m3 node at nixpkgs-unstable
# (nix-channel --add https://nixos.org/channels/nixpkgs-unstable nixpkgs)
# to get the newest base packages before our overlay applies.
{ ... }@args:
import <nixpkgs> (args // {
  overlays = [
    (import ../overlays/python-optimized.nix) # python315t-optimized
    (import ../overlays/node-optimized.nix)   # node-optimized (Node.js 26 + ThinLTO)
  ];
})
