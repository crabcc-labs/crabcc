#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/version.sh
#
# Single source of truth for the workspace version. Other scripts should
# either source this file (`. scripts/version.sh` → exposes
# `$CRABCC_VERSION`) or call it as a command (prints the bare version).
#
# Reads `[workspace.package].version` from Cargo.toml. The build system,
# release tooling, doctor / check-deps banners, and release notes all
# read through here, so bumping the workspace version flips them
# automatically.
#
# Usage:
#   scripts/version.sh             # prints version (e.g. "2.2.0")
#   scripts/version.sh --json      # {"version":"2.2.0","cargo_toml":"..."}
#   . scripts/version.sh           # exports CRABCC_VERSION + helpers
#
# Exit codes:
#   0  version found
#   1  Cargo.toml missing or version line missing
#
# ---------------------------------------------------------------------------
# CHANGELOG
#   v1.0.0 (2026-04-30) — initial cut. Powers `task version` and the
#                          banners in check-deps.sh / doctor.sh.
# ---------------------------------------------------------------------------

# When sourced (`. scripts/version.sh`) we need to *not* set -e on the
# caller's shell. Bash exposes BASH_SOURCE[0] != $0 only when sourced.
__crabcc_is_sourced=0
[ "${BASH_SOURCE[0]:-$0}" != "$0" ] && __crabcc_is_sourced=1

# Locate the workspace root. We walk up from this script's directory to
# the first ancestor that owns a Cargo.toml carrying `[workspace]`. This
# is robust to CWD because `task` and other invocations may run us from
# subdirectories.
crabcc_workspace_root() {
    local d
    d="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
    while [ "$d" != "/" ]; do
        if [ -f "$d/Cargo.toml" ] && grep -q '^\[workspace\]' "$d/Cargo.toml"; then
            echo "$d"
            return 0
        fi
        d="$(dirname "$d")"
    done
    return 1
}

# Parse the version. Three fallbacks in priority order:
#   1. `cargo metadata --format-version 1 | jq` if both available
#   2. awk on Cargo.toml (no deps)
#   3. grep + sed (last resort)
# Each prints just the bare version, e.g. "2.2.0".
crabcc_version() {
    local root
    root="$(crabcc_workspace_root)" || return 1
    local cargo_toml="$root/Cargo.toml"
    [ -f "$cargo_toml" ] || return 1

    # Awk: scan from `[workspace.package]` until next `[…]` heading and
    # pluck the `version = "x.y.z"` line. Robust to comments and ordering
    # tweaks because it only acts inside the right section.
    local v
    v="$(awk '
        /^\[workspace\.package\]/ { in_section = 1; next }
        /^\[/                     { in_section = 0 }
        in_section && /^[[:space:]]*version[[:space:]]*=/ {
            match($0, /"[^"]+"/)
            if (RLENGTH > 0) {
                print substr($0, RSTART + 1, RLENGTH - 2)
                exit
            }
        }
    ' "$cargo_toml")"

    # Fall back to the `[package]` block — used by single-crate setups
    # before they grow into a workspace.
    if [ -z "$v" ]; then
        v="$(awk '
            /^\[package\]/ { in_section = 1; next }
            /^\[/          { in_section = 0 }
            in_section && /^[[:space:]]*version[[:space:]]*=/ {
                match($0, /"[^"]+"/)
                if (RLENGTH > 0) {
                    print substr($0, RSTART + 1, RLENGTH - 2)
                    exit
                }
            }
        ' "$cargo_toml")"
    fi

    [ -n "$v" ] || return 1
    printf '%s' "$v"
}

# When sourced, expose CRABCC_VERSION + the helpers. When run directly,
# print the version (or JSON with --json).
if [ "$__crabcc_is_sourced" = "1" ]; then
    if v="$(crabcc_version)"; then
        export CRABCC_VERSION="$v"
        export CRABCC_WORKSPACE_ROOT="$(crabcc_workspace_root)"
    fi
else
    case "${1:-}" in
        --json)
            v="$(crabcc_version)" || { echo "{\"error\":\"version not found\"}" >&2; exit 1; }
            r="$(crabcc_workspace_root)"
            printf '{"version":"%s","cargo_toml":"%s/Cargo.toml"}\n' "$v" "$r"
            ;;
        --help|-h)
            sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'
            ;;
        *)
            v="$(crabcc_version)" || { echo "version not found in Cargo.toml" >&2; exit 1; }
            printf '%s\n' "$v"
            ;;
    esac
fi
