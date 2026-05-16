#!/opt/homebrew/bin/bash
# dispatch-rotated.sh — wrap run-task.sh with per-task model rotation,
# preflight checks, auto-cleanup of stale worktree state, and a tee'd log.
#
# Usage:
#   tools/orchestrator/dispatch-rotated.sh <plan-name> <task-id> [<task-id>...]
#   tools/orchestrator/dispatch-rotated.sh --preflight <plan-name> <task-id> [<task-id>...]
#
# Rotates ORCH_CODER_MODEL across MODELS indexed by task_id % len(MODELS).
# Each task runs in its own ~/.orchestrator worktree under a per-id semaphore
# slot (ORCH_MAX_CONCURRENT, default 10).
#
# Bulletproofing:
# - Hardcoded shebang to /opt/homebrew/bin/bash (declare -A needs bash 5+).
# - Prepends ~/.opencode/bin to PATH (opencode installs there and is rarely
#   on the default non-login PATH).
# - Auto-sets ORCH_BASE_BRANCH to the current branch unless explicitly set,
#   so worktree base and allow-list diff base always match the integrator.
# - Preflight verifies all deps (bash5, opencode, jq, gtimeout, git, run-task.sh,
#   plan dir, per-id prompt+allowlist) before dispatching anything.
# - Auto-removes stale worktrees and branches for the task IDs being dispatched
#   so re-runs after a failure don't trip "branch already exists" / "worktree
#   path already registered".
# - On failure, prints the last 12 lines of each failed task's coder log so the
#   diagnosis is in the dispatch output.
# - Dispatch output is tee'd to ~/.orchestrator/dispatch-logs/<plan>-<ts>.log
#   so the trail survives even if the caller pipes our stdout through tail.

set -euo pipefail

# ── Environment normalisation ────────────────────────────────────────────────
export PATH="$HOME/.opencode/bin:$PATH"
export ORCH_BASE_BRANCH="${ORCH_BASE_BRANCH:-$(git -C "$(git rev-parse --show-toplevel)" rev-parse --abbrev-ref HEAD)}"

MODELS=(
    "openrouter/tencent/hy3-preview"
    "openrouter/deepseek/deepseek-v4-flash"
    "openrouter/deepseek/deepseek-v3.2-exp"
    "openrouter/deepseek/deepseek-v4-pro"
)

ORCH_MAX_CONCURRENT="${ORCH_MAX_CONCURRENT:-10}"
ORCH_LAUNCH_STAGGER_SECONDS="${ORCH_LAUNCH_STAGGER_SECONDS:-0.5}"
ORCH_REVIEWER_MODEL="${ORCH_REVIEWER_MODEL:-openrouter/deepseek/deepseek-v4-flash}"
ORCH_RUNTIME="${ORCH_RUNTIME:-$HOME/.orchestrator}"

# ── Argument parsing ─────────────────────────────────────────────────────────
PREFLIGHT_ONLY=0
if [[ "${1:-}" == "--preflight" ]]; then
    PREFLIGHT_ONLY=1
    shift
fi

if [[ $# -lt 2 ]]; then
    echo "usage: $0 [--preflight] <plan> <id> [<id>...]" >&2
    exit 1
fi

PLAN="$1"
shift
IDS=("$@")

REPO="$(git rev-parse --show-toplevel)"
RUN_TASK="${ORCH_RUN_TASK_SCRIPT:-$REPO/tools/orchestrator/run-task.sh}"
PLAN_DIR="$REPO/tools/orchestrator/plans/$PLAN"

# ── Preflight ────────────────────────────────────────────────────────────────
preflight() {
    local errors=0
    local warn=0
    local _

    # bash5: declare -A is a fatal smoke test
    if ! declare -A _ 2>/dev/null; then
        echo "  ✗ bash 5+ required (declare -A); got $BASH_VERSION" >&2
        ((errors++))
    fi

    for dep in opencode jq gtimeout shlock git; do
        if ! command -v "$dep" >/dev/null 2>&1; then
            echo "  ✗ missing dep: $dep" >&2
            ((errors++))
        fi
    done

    if [[ ! -x "$RUN_TASK" ]]; then
        echo "  ✗ run-task.sh not executable: $RUN_TASK" >&2
        ((errors++))
    fi

    if [[ ! -d "$PLAN_DIR" ]]; then
        echo "  ✗ plan dir missing: $PLAN_DIR" >&2
        ((errors++))
    fi

    # Worktree base must be a real ref.
    if ! git -C "$REPO" rev-parse --verify --quiet "$ORCH_BASE_BRANCH" >/dev/null; then
        echo "  ✗ ORCH_BASE_BRANCH '$ORCH_BASE_BRANCH' is not a valid git ref" >&2
        ((errors++))
    fi

    # No in-progress cherry-pick / rebase / merge — those will trip cherry-picks after.
    if [[ -e "$REPO/.git/CHERRY_PICK_HEAD" || -d "$REPO/.git/rebase-merge" || -d "$REPO/.git/rebase-apply" ]]; then
        echo "  ⚠ in-progress git operation detected — finish it first" >&2
        ((warn++))
    fi

    # Per-task: prompt and allowlist files must exist.
    for tid in "${IDS[@]}"; do
        [[ -f "$PLAN_DIR/prompts/task-$tid.md" ]] \
            || { echo "  ✗ missing prompt: $PLAN_DIR/prompts/task-$tid.md" >&2; ((errors++)); }
        [[ -f "$PLAN_DIR/allowlists/task-$tid.txt" ]] \
            || { echo "  ✗ missing allowlist: $PLAN_DIR/allowlists/task-$tid.txt" >&2; ((errors++)); }
    done

    if [[ $errors -gt 0 ]]; then
        echo "preflight: FAIL ($errors errors, $warn warnings)" >&2
        exit 10
    fi
    echo "preflight: OK ($warn warnings)"
}

# ── Auto-cleanup of stale state ──────────────────────────────────────────────
cleanup_stale_for_ids() {
    for tid in "${IDS[@]}"; do
        local wt="$ORCH_RUNTIME/worktrees/$PLAN-task-$tid"
        local br="wave/$PLAN/task-$tid"
        local pid_file="$ORCH_RUNTIME/pids/$PLAN-task-$tid.pid"
        local lock="$ORCH_RUNTIME/locks/$PLAN-task-$tid.lock"

        if [[ -d "$wt" ]] || git -C "$REPO" show-ref --verify --quiet "refs/heads/$br" 2>/dev/null; then
            echo "  cleanup task-$tid: removing stale worktree + branch"
            git -C "$REPO" worktree remove --force "$wt" 2>/dev/null || true
            rm -rf "$wt"
            git -C "$REPO" branch -D "$br" 2>/dev/null || true
        fi
        rm -f "$pid_file" "$lock" 2>/dev/null || true
    done
    git -C "$REPO" worktree prune >/dev/null 2>&1 || true
}

# ── Dispatch log (tee'd, survives stdout pipes) ──────────────────────────────
mkdir -p "$ORCH_RUNTIME/dispatch-logs"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
DISPATCH_LOG="$ORCH_RUNTIME/dispatch-logs/$PLAN-$TS.log"

# Print headers (also to log via tee at the end).
{
    echo "dispatch-rotated: plan=$PLAN tasks=${IDS[*]}"
    echo "  base=$ORCH_BASE_BRANCH  concurrency=$ORCH_MAX_CONCURRENT"
    echo "  models: ${MODELS[*]}"
    echo "  reviewer: $ORCH_REVIEWER_MODEL"
    echo "  log: $DISPATCH_LOG"
    echo

    echo "[preflight]"
    preflight
    if [[ $PREFLIGHT_ONLY -eq 1 ]]; then
        echo "preflight-only mode — exiting before dispatch"
        exit 0
    fi
    echo

    echo "[cleanup-stale]"
    cleanup_stale_for_ids
    echo
} | tee -a "$DISPATCH_LOG"

# ── Dispatch loop (per-id semaphore + model rotation) ────────────────────────
PIPE=$(mktemp -u)
mkfifo "$PIPE"
exec 3<>"$PIPE"
rm "$PIPE"
for ((i=0; i<ORCH_MAX_CONCURRENT; i++)); do echo >&3; done

declare -A PID_TO_TASK=()
declare -A PID_TO_MODEL=()

{
    echo "[dispatch]"
    for tid in "${IDS[@]}"; do
        read -u 3
        midx=$((tid % ${#MODELS[@]}))
        model="${MODELS[$midx]}"
        sleep "$(awk -v m="$ORCH_LAUNCH_STAGGER_SECONDS" 'BEGIN { srand(); print rand()*m }')"
        (
            ORCH_CODER_MODEL="$model" \
            ORCH_REVIEWER_MODEL="$ORCH_REVIEWER_MODEL" \
            "$RUN_TASK" --plan "$PLAN" "$tid"
            ec=$?
            echo >&3
            exit $ec
        ) &
        pid=$!
        PID_TO_TASK[$pid]=$tid
        PID_TO_MODEL[$pid]=$model
        echo "  task-$tid → pid=$pid model=$model"
    done
} | tee -a "$DISPATCH_LOG"

# ── Wait + summarise ─────────────────────────────────────────────────────────
ok=0; fail=0; failed_ids=()
declare -A TASK_EXIT=()
for pid in "${!PID_TO_TASK[@]}"; do
    if wait "$pid"; then
        ok=$((ok+1))
        echo "  task-${PID_TO_TASK[$pid]}  OK    (${PID_TO_MODEL[$pid]})" | tee -a "$DISPATCH_LOG"
    else
        ec=$?
        fail=$((fail+1))
        failed_ids+=("${PID_TO_TASK[$pid]}")
        TASK_EXIT[${PID_TO_TASK[$pid]}]=$ec
        echo "  task-${PID_TO_TASK[$pid]}  FAILED exit=$ec  (${PID_TO_MODEL[$pid]})" | tee -a "$DISPATCH_LOG"
    fi
done

{
    echo
    echo "summary: ok=$ok fail=$fail total=${#IDS[@]}"
    if [[ $fail -gt 0 ]]; then
        echo "failed: ${failed_ids[*]}"
        echo
        echo "[failure tails — last 12 lines of each failed task's coder log]"
        for tid in "${failed_ids[@]}"; do
            local_log="$(ls -t "$ORCH_RUNTIME/logs/$PLAN-task-$tid-coder-"*.log 2>/dev/null | head -1)"
            echo "── task-$tid (exit ${TASK_EXIT[$tid]}) — $local_log"
            if [[ -n "$local_log" && -f "$local_log" ]]; then
                tail -12 "$local_log" | sed 's/^/   │ /'
            else
                echo "   │ (no coder log found — task likely failed before opencode launched)"
            fi
            echo
        done
    fi
    echo "log: $DISPATCH_LOG"
} | tee -a "$DISPATCH_LOG"

[[ $fail -eq 0 ]]
