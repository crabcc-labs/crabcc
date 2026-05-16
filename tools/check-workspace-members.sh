#!/usr/bin/env bash
# tools/check-workspace-members.sh
#
# Assert that every Rust crate discovered on disk under crates/ and apps/
# is explicitly accounted for in the root Cargo.toml — either as a workspace
# member or in the `exclude = [...]` array.
#
# Guards against cargo issue #9853: when a new crate directory is added,
# it can be silently picked up (or silently missed) without an explicit
# declaration. This script makes the allow-list mandatory.
#
# Usage:
#   bash tools/check-workspace-members.sh        # from repo root
#   task check:workspace-members
#
# Exit 0 if all discovered crates are accounted for.
# Exit 1 and print a clear error listing undeclared crates otherwise.
#
# Dependencies: cargo, jq (both available in CI and dev environments).
# Compatible with bash 3.2+ (macOS system bash).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_CARGO="$REPO_ROOT/Cargo.toml"

# ---------------------------------------------------------------------------
# 1. Discover every Rust crate on disk under crates/ and apps/.
#    A "crate" is any immediate subdirectory (depth 1) that contains a
#    Cargo.toml. We do not descend into sub-crates (e.g. crates/foo/fuzz/).
# ---------------------------------------------------------------------------
discovered_file=$(mktemp)
trap 'rm -f "$discovered_file"' EXIT

for search_dir in "$REPO_ROOT/crates" "$REPO_ROOT/apps"; do
    [[ -d "$search_dir" ]] || continue
    find "$search_dir" -mindepth 2 -maxdepth 2 -name "Cargo.toml" \
        | sed "s|$REPO_ROOT/||; s|/Cargo.toml$||" \
        >> "$discovered_file"
done
sort -o "$discovered_file" "$discovered_file"

if [[ ! -s "$discovered_file" ]]; then
    echo "check-workspace-members: no crates found under crates/ or apps/ — nothing to check" >&2
    exit 0
fi

# ---------------------------------------------------------------------------
# 2. Get the canonical resolved member list via cargo metadata.
#    manifest_path is absolute; strip repo root prefix and /Cargo.toml.
# ---------------------------------------------------------------------------
members_file=$(mktemp)
trap 'rm -f "$discovered_file" "$members_file"' EXIT

cargo metadata --no-deps --format-version=1 --manifest-path "$ROOT_CARGO" \
    | jq -r '.packages[].manifest_path' \
    | sed "s|$REPO_ROOT/||; s|/Cargo.toml$||" \
    | sort > "$members_file"

# ---------------------------------------------------------------------------
# 3. Parse the `exclude = [...]` array from the root Cargo.toml.
#    awk collects lines between `exclude = [` and the closing `]`, then
#    strips quotes and commas. Works for plain string values only —
#    workspace exclude paths are always plain strings.
# ---------------------------------------------------------------------------
excluded_file=$(mktemp)
trap 'rm -f "$discovered_file" "$members_file" "$excluded_file"' EXIT

awk '
    /^exclude[[:space:]]*=/ { in_exclude=1; next }
    in_exclude && /\]/ { in_exclude=0; next }
    in_exclude && /"/ {
        line=$0
        gsub(/^[[:space:]]*"/, "", line)
        gsub(/"[[:space:],]*$/, "", line)
        if (line != "") print line
    }
' "$ROOT_CARGO" | sort > "$excluded_file"

# ---------------------------------------------------------------------------
# 4. Build the combined "known" set = members union excluded.
# ---------------------------------------------------------------------------
known_file=$(mktemp)
trap 'rm -f "$discovered_file" "$members_file" "$excluded_file" "$known_file"' EXIT
sort -m "$members_file" "$excluded_file" | sort -u > "$known_file"

# ---------------------------------------------------------------------------
# 5. Diff: every discovered crate must appear in the known set.
# ---------------------------------------------------------------------------
undeclared=$(comm -23 "$discovered_file" "$known_file")

# ---------------------------------------------------------------------------
# 6. Also check the inverse: members that no longer exist on disk (stale).
# ---------------------------------------------------------------------------
stale=""
while IFS= read -r m; do
    if [[ ! -f "$REPO_ROOT/$m/Cargo.toml" ]]; then
        stale="${stale}  $m"$'\n'
    fi
done < "$members_file"

# ---------------------------------------------------------------------------
# 7. Report.
# ---------------------------------------------------------------------------
ok=0

if [[ -n "$undeclared" ]]; then
    echo "check-workspace-members: FAIL — undeclared crate(s) found" >&2
    echo "" >&2
    echo "These directories contain a Cargo.toml but appear in neither" >&2
    echo "the workspace members nor the exclude array in Cargo.toml:" >&2
    echo "" >&2
    echo "$undeclared" | sed 's/^/  /' >&2
    echo "" >&2
    echo "Fix: add each path to either [workspace] members = [...] or" >&2
    echo "exclude = [...] in the root Cargo.toml." >&2
    echo "(Cargo issue #9853 — silent auto-discovery can pull these in" >&2
    echo " or silently drop them from builds without this guard.)" >&2
    ok=1
fi

if [[ -n "$stale" ]]; then
    echo "check-workspace-members: WARN — stale workspace member(s) (no Cargo.toml on disk):" >&2
    printf '%s' "$stale" >&2
    echo "Fix: remove from members in the root Cargo.toml." >&2
    ok=1
fi

if [[ $ok -eq 0 ]]; then
    n_discovered=$(wc -l < "$discovered_file" | tr -d ' ')
    n_members=$(wc -l < "$members_file" | tr -d ' ')
    n_excluded=$(wc -l < "$excluded_file" | tr -d ' ')
    echo "check-workspace-members: ok — all ${n_discovered} crate(s) accounted for"
    echo "  members : ${n_members}"
    echo "  excluded: ${n_excluded}"
fi

exit $ok
