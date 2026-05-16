#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/install-hooks.sh
#
# Install local Git hooks. Today we ship a single `pre-commit` hook that
# runs the fast gate (cargo fmt + clippy --lib --bins + aliases-smoke).
#
# Strategy:
#   * Symlink (not copy) `.git/hooks/pre-commit` → `scripts/git-hooks/pre-commit`
#     so updates to the tracked hook propagate automatically.
#   * If `core.hooksPath` is set elsewhere, install into that path instead.
#   * Idempotent: replaces any existing symlink pointing at our hook;
#     refuses to overwrite an unrelated hand-written hook unless --force.
#
# Usage:
#   scripts/install-hooks.sh              # install symlink
#   scripts/install-hooks.sh --force      # overwrite an existing hook
#   scripts/install-hooks.sh --remove     # uninstall the symlink
#   scripts/install-hooks.sh --print      # show what would happen, don't modify
# ---------------------------------------------------------------------------

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SOURCE_HOOK="$REPO_ROOT/scripts/git-hooks/pre-commit"

ACTION="install"
FORCE=0
for arg in "$@"; do
    case "$arg" in
        --force)   FORCE=1 ;;
        --remove)  ACTION="remove" ;;
        --print)   ACTION="print" ;;
        --help|-h)
            sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "unknown arg: $arg" >&2; exit 2 ;;
    esac
done

[ -f "$SOURCE_HOOK" ] || { echo "missing: $SOURCE_HOOK" >&2; exit 1; }
chmod +x "$SOURCE_HOOK" 2>/dev/null || true

# Resolve the actual hooks dir — honors core.hooksPath if the user set it.
# `git rev-parse --git-dir` returns the per-worktree git dir
# (`.git/worktrees/<name>/` for worktrees, plain `.git/` for the main
# checkout) — this is what makes the script worktree-safe.
HOOKS_PATH="$(git -C "$REPO_ROOT" config core.hooksPath 2>/dev/null || true)"
if [ -z "$HOOKS_PATH" ]; then
    GIT_DIR="$(git -C "$REPO_ROOT" rev-parse --git-dir)"
    case "$GIT_DIR" in
        /*) HOOKS_PATH="$GIT_DIR/hooks" ;;
        *)  HOOKS_PATH="$REPO_ROOT/$GIT_DIR/hooks" ;;
    esac
elif [ "${HOOKS_PATH:0:1}" != "/" ]; then
    HOOKS_PATH="$REPO_ROOT/$HOOKS_PATH"
fi

TARGET="$HOOKS_PATH/pre-commit"

case "$ACTION" in
    print)
        echo "would symlink: $TARGET → $SOURCE_HOOK"
        ;;
    remove)
        if [ -L "$TARGET" ]; then
            rm -f "$TARGET"
            echo "removed symlink: $TARGET"
        elif [ -f "$TARGET" ]; then
            echo "refusing to remove non-symlink hook: $TARGET" >&2
            echo "(if you installed crabcc's hook by copy, delete it manually)" >&2
            exit 1
        else
            echo "no hook installed at $TARGET"
        fi
        ;;
    install)
        mkdir -p "$HOOKS_PATH"
        if [ -e "$TARGET" ] || [ -L "$TARGET" ]; then
            # Already pointing at our hook? Idempotent refresh.
            link_target="$(readlink "$TARGET" 2>/dev/null || true)"
            if [ "$link_target" = "$SOURCE_HOOK" ]; then
                echo "pre-commit symlink already in place: $TARGET"
                exit 0
            fi
            if [ "$FORCE" = "0" ]; then
                echo "existing hook at $TARGET (run with --force to overwrite)" >&2
                exit 1
            fi
            rm -f "$TARGET"
        fi
        ln -s "$SOURCE_HOOK" "$TARGET"
        echo "installed pre-commit symlink: $TARGET → $SOURCE_HOOK"
        echo "skip with: CRABCC_SKIP_HOOKS=1 git commit ..."
        ;;
esac
