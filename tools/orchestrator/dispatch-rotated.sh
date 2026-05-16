#!/opt/homebrew/bin/bash
# dispatch-rotated.sh — wrap orch-dispatch-wave with per-task model rotation.
#
# Usage:
#   tools/orchestrator/dispatch-rotated.sh <plan-name> <task-id> [<task-id>...]
#
# Rotates ORCH_CODER_MODEL across the MODELS array, indexed by task id
# modulo array length. So task 5 with a 4-model array uses MODELS[5%4]=1.
# Calls run-task.sh in parallel using the same semaphore as orch-dispatch-wave.

set -euo pipefail

# Ensure opencode is on PATH regardless of how this script is invoked.
# opencode installs into ~/.opencode/bin which is not on the default
# non-login shell PATH.
export PATH="$HOME/.opencode/bin:$PATH"

MODELS=(
    "openrouter/tencent/hy3-preview"
    "openrouter/deepseek/deepseek-v4-flash"
    "openrouter/deepseek/deepseek-v3.2-exp"
    "openrouter/deepseek/deepseek-v4-pro"
)

ORCH_MAX_CONCURRENT="${ORCH_MAX_CONCURRENT:-10}"
ORCH_LAUNCH_STAGGER_SECONDS="${ORCH_LAUNCH_STAGGER_SECONDS:-0.5}"
ORCH_REVIEWER_MODEL="${ORCH_REVIEWER_MODEL:-openrouter/deepseek/deepseek-v4-flash}"

PLAN="$1"
shift
IDS=("$@")

if [[ ${#IDS[@]} -eq 0 ]]; then
    echo "usage: $0 <plan> <id> [<id>...]" >&2
    exit 1
fi

REPO="$(git rev-parse --show-toplevel)"
RUN_TASK="${ORCH_RUN_TASK_SCRIPT:-$REPO/tools/orchestrator/run-task.sh}"

if [[ ! -x "$RUN_TASK" ]]; then
    echo "run-task.sh not found or not executable: $RUN_TASK" >&2
    exit 2
fi

echo "dispatch-rotated: plan=$PLAN tasks=${IDS[*]} concurrency=$ORCH_MAX_CONCURRENT"
echo "  models: ${MODELS[*]}"

# Named-pipe semaphore (same pattern as orch-dispatch-wave).
PIPE=$(mktemp -u)
mkfifo "$PIPE"
exec 3<>"$PIPE"
rm "$PIPE"
for ((i=0; i<ORCH_MAX_CONCURRENT; i++)); do echo >&3; done

declare -A PID_TO_TASK=()
declare -A PID_TO_MODEL=()
declare -A TASK_EXIT=()

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

# Wait + collect exit codes.
ok=0; fail=0; failed_ids=()
for pid in "${!PID_TO_TASK[@]}"; do
    if wait "$pid"; then
        ok=$((ok+1))
        echo "  task-${PID_TO_TASK[$pid]}  OK    (${PID_TO_MODEL[$pid]})"
    else
        ec=$?
        fail=$((fail+1))
        failed_ids+=("${PID_TO_TASK[$pid]}")
        TASK_EXIT[${PID_TO_TASK[$pid]}]=$ec
        echo "  task-${PID_TO_TASK[$pid]}  FAILED exit=$ec  (${PID_TO_MODEL[$pid]})"
    fi
done

echo
echo "summary: ok=$ok fail=$fail total=${#IDS[@]}"
if [[ $fail -gt 0 ]]; then
    echo "failed: ${failed_ids[*]}"
    exit 1
fi
