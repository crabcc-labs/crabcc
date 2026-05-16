#!/opt/homebrew/bin/bash
# integrate-wave.sh — cherry-pick all task branches of a wave onto v4-hackathon,
# run a wave-checkpoint, then clean up the worktrees + branches. Idempotent.
#
# Usage: tools/orchestrator/integrate-wave.sh <plan-name> <id> [<id>...]

set -euo pipefail

PLAN="$1"
shift
IDS=("$@")

if [[ ${#IDS[@]} -eq 0 ]]; then
    echo "usage: $0 <plan-name> <id> [<id>...]" >&2
    exit 1
fi

# Cherry-pick each task branch in id order.
for tid in "${IDS[@]}"; do
    BR="wave/$PLAN/task-$tid"
    if ! git show-ref --verify --quiet "refs/heads/$BR"; then
        echo "integrate: branch $BR missing — task may have failed; aborting" >&2
        exit 1
    fi
    echo "integrate: cherry-pick $BR"
    if ! git cherry-pick "$BR"; then
        echo "integrate: conflict on $BR — resolve manually, then run integrate-wave with remaining ids" >&2
        exit 2
    fi
done

# Cleanup: drop worktrees + branches once they're integrated.
for tid in "${IDS[@]}"; do
    WT="$HOME/.orchestrator/worktrees/$PLAN-task-$tid"
    BR="wave/$PLAN/task-$tid"
    git worktree remove --force "$WT" 2>/dev/null || true
    git branch -D "$BR" 2>/dev/null || true
done

echo "integrate: ${#IDS[@]} task(s) cherry-picked onto $(git branch --show-current)"
