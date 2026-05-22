#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/install-hooks.sh
#
# Install local Git hooks. Installs symlinks for every hook in
# scripts/git-hooks/ (pre-commit, commit-msg, pre-push).
#
# Hooks installed:
#   pre-commit  — cargo fmt (autofix + restage), clippy, TS typecheck,
#                 actionlint, openapi.yaml validation, image compression
#   commit-msg  — Conventional Commits format enforcement
#   pre-push    — scoped cargo nextest for crates touched by pushed commits
#
# Strategy:
#   * Symlink (not copy) `.git/hooks/<name>` → `scripts/git-hooks/<name>`
#     so updates to tracked hooks propagate automatically.
#   * If `core.hooksPath` is set elsewhere, install into that path instead.
#   * Idempotent: replaces any existing symlink pointing at our hook;
#     refuses to overwrite an unrelated hand-written hook unless --force.
#
# Usage:
#   scripts/install-hooks.sh              # install all hooks
#   scripts/install-hooks.sh --force      # overwrite existing hooks
#   scripts/install-hooks.sh --remove     # uninstall all symlinks
#   scripts/install-hooks.sh --print      # show what would happen, don't modify
#
# Bypass at commit time:  CRABCC_SKIP_HOOKS=1 git commit ...
#                         git commit --no-verify
# ---------------------------------------------------------------------------

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HOOKS_SRC_DIR="$REPO_ROOT/scripts/git-hooks"

ACTION="install"
FORCE=0
for arg in "$@"; do
    case "$arg" in
        --force)   FORCE=1 ;;
        --remove)  ACTION="remove" ;;
        --print)   ACTION="print" ;;
        --help|-h)
            sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "unknown arg: $arg" >&2; exit 2 ;;
    esac
done

# Collect the hooks we ship (everything in scripts/git-hooks/ that's executable
# or a plain file — excludes README, *.md, etc.).
HOOKS=()
for f in "$HOOKS_SRC_DIR"/*; do
    [ -f "$f" ] || continue
    case "$(basename "$f")" in *.md|*.txt|*.example) continue ;; esac
    HOOKS+=("$(basename "$f")")
done

[ "${#HOOKS[@]}" -gt 0 ] || { echo "no hooks found in $HOOKS_SRC_DIR" >&2; exit 1; }

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

mkdir -p "$HOOKS_PATH"

exit_code=0
for hook in "${HOOKS[@]}"; do
    SOURCE_HOOK="$HOOKS_SRC_DIR/$hook"
    TARGET="$HOOKS_PATH/$hook"
    chmod +x "$SOURCE_HOOK" 2>/dev/null || true

    case "$ACTION" in
        print)
            echo "would symlink: $TARGET → $SOURCE_HOOK"
            ;;
        remove)
            if [ -L "$TARGET" ]; then
                rm -f "$TARGET"
                echo "removed: $TARGET"
            elif [ -f "$TARGET" ]; then
                echo "refusing to remove non-symlink hook: $TARGET" >&2
                echo "(if you installed crabcc's hook by copy, delete it manually)" >&2
                exit_code=1
            else
                echo "not installed: $TARGET"
            fi
            ;;
        install)
            if [ -e "$TARGET" ] || [ -L "$TARGET" ]; then
                link_target="$(readlink "$TARGET" 2>/dev/null || true)"
                if [ "$link_target" = "$SOURCE_HOOK" ]; then
                    echo "already in place: $TARGET"
                    continue
                fi
                if [ "$FORCE" = "0" ]; then
                    echo "existing hook at $TARGET (run with --force to overwrite)" >&2
                    exit_code=1
                    continue
                fi
                rm -f "$TARGET"
            fi
            ln -s "$SOURCE_HOOK" "$TARGET"
            echo "installed: $TARGET → $SOURCE_HOOK"
            ;;
    esac
done

if [ "$ACTION" = "install" ] && [ "$exit_code" = "0" ]; then
    echo ""
    echo "Hooks active: ${HOOKS[*]}"
    echo "Bypass: CRABCC_SKIP_HOOKS=1 git commit|push ..."
    echo "        git commit --no-verify | git push --no-verify"
fi

exit "$exit_code"
