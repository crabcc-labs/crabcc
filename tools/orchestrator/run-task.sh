#!/usr/bin/env bash
# run-task.sh — run one plan task in an isolated git worktree, with a Kimi K2.6
# coder pass, path-allowlist enforcement, and a DeepSeek V4 Flash reviewer pass.
#
# Usage: run-task.sh --plan <plan-name> <task-id>
#
# Reads (under the repo root):
#   tools/orchestrator/plans/<plan-name>/prompts/task-<id>.md
#   tools/orchestrator/plans/<plan-name>/allowlists/task-<id>.txt   (one path or prefix per line)
#
# Writes (under ~/.orchestrator by default, kept outside the repo):
#   ~/.orchestrator/logs/<plan>-task-<id>-coder-<ts>.log
#   ~/.orchestrator/logs/<plan>-task-<id>-review-<ts>.log
#   ~/.orchestrator/pids/<plan>-task-<id>.pid                       (live process PID; removed on exit)
#   ~/.orchestrator/worktrees/<plan>-task-<id>/                     (branch wave/<plan>/task-<id>)
#
# Environment overrides:
#   ORCH_RUNTIME                  runtime dir (default: ~/.orchestrator)
#   ORCH_CODER_MODEL              opencode model spec (default: openrouter/moonshotai/kimi-k2.6)
#   ORCH_REVIEWER_MODEL           opencode model spec (default: openrouter/deepseek/deepseek-v4-flash)
#   ORCH_TIMEOUT_SECONDS          coder hard timeout in seconds (default: 1200)
#   ORCH_LAUNCH_STAGGER_SECONDS   max random sleep [0, N] before first API call (default: 1.0)
#   ORCH_NOTIFY_ON_FAILURE        1 to fire a macOS notification on non-zero exit (default: 1)
#                                 Prefers terminal-notifier (brew install terminal-notifier);
#                                 falls back to osascript if terminal-notifier is missing.
#   ORCH_TASK_RETRIES             max retries on transient coder failure (default: 0)
#                                 Does NOT retry timeouts (exit 40). On retry, the worktree
#                                 is reset --hard to main + clean -fd so the next attempt
#                                 starts from a clean slate.
#
# Reviewer: as of v0.4, the per-task inline reviewer pass is replaced by a
# long-running daemon (tools/orchestrator/review-daemon.sh). Each finished
# task appends a single envelope JSON line to ~/.orchestrator/review-queue.jsonl;
# the daemon consumes them serially. If no daemon is running, tasks still
# complete normally; their envelopes accumulate in the queue until you
# start one. See `tools/orchestrator/review-daemon.sh status`.
#
# Exit codes:
#   0  OK (commit landed, diff inside allow-list, review captured)
#  10  prerequisites missing
#  20  worktree creation failed
#  30  lock already held
#  40  opencode timed out
#  50  opencode exited non-zero
#  60  no commit produced
#  70  diff escaped the allow-list

set -uo pipefail

PLAN=""
TASK_ID=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --plan) PLAN="$2"; shift 2 ;;
        --) shift; break ;;
        -*) echo "unknown flag: $1" >&2; exit 10 ;;
        *) TASK_ID="$1"; shift ;;
    esac
done
if [[ -z "$PLAN" || -z "$TASK_ID" ]]; then
    echo "usage: $0 --plan <plan-name> <task-id>" >&2
    exit 10
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null)"
if [[ -z "$REPO" ]]; then
    echo "run-task.sh must live inside a git repo" >&2
    exit 10
fi

ORCH_RUNTIME="${ORCH_RUNTIME:-$HOME/.orchestrator}"
mkdir -p "$ORCH_RUNTIME"/{worktrees,locks,logs,pids}

# Pre-flight disk check: refuse to start if free space under ORCH_RUNTIME
# is below 2GB. 10 iOS-source worktrees + logs adds up fast.
AVAIL_KB="$(df -k "$ORCH_RUNTIME" 2>/dev/null | awk 'NR==2 {print $4}')"
if [[ "${AVAIL_KB:-0}" =~ ^[0-9]+$ ]] && [[ "${AVAIL_KB:-0}" -lt 524288 ]]; then
    echo "[task $TASK_ID] only ${AVAIL_KB}KB free in $ORCH_RUNTIME — need ≥2GB" >&2
    exit 10
fi

PROMPT="$REPO/tools/orchestrator/plans/$PLAN/prompts/task-$TASK_ID.md"
ALLOWLIST="$REPO/tools/orchestrator/plans/$PLAN/allowlists/task-$TASK_ID.txt"
WORKTREE="$ORCH_RUNTIME/worktrees/$PLAN-task-$TASK_ID"
BRANCH="wave/$PLAN/task-$TASK_ID"
LOCK="$ORCH_RUNTIME/locks/$PLAN-task-$TASK_ID.lock"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
CODER_LOG="$ORCH_RUNTIME/logs/$PLAN-task-$TASK_ID-coder-$TS.log"
REVIEW_LOG="$ORCH_RUNTIME/logs/$PLAN-task-$TASK_ID-review-$TS.log"

CODER_MODEL="${ORCH_CODER_MODEL:-openrouter/moonshotai/kimi-k2.6}"
REVIEWER_MODEL="${ORCH_REVIEWER_MODEL:-openrouter/deepseek/deepseek-v4-flash}"
TIMEOUT_SECONDS="${ORCH_TIMEOUT_SECONDS:-1200}"
LAUNCH_STAGGER_SECONDS="${ORCH_LAUNCH_STAGGER_SECONDS:-1.0}"
NOTIFY_ON_FAILURE="${ORCH_NOTIFY_ON_FAILURE:-1}"
PIDFILE="$ORCH_RUNTIME/pids/$PLAN-task-$TASK_ID.pid"
WT_LOCK="$ORCH_RUNTIME/locks/.worktree-create.lock"

notify_failure() {
    local code="$1"
    [[ "$NOTIFY_ON_FAILURE" != "1" ]] && return 0
    local title="Orchestrator: $PLAN task $TASK_ID failed"
    local body="Exit $code. Log: ${CODER_LOG:-<no log>}"
    local icon="$REPO/tools/orchestrator/assets/notify-icon.png"
    # Psy trance build-up: four chained system sounds, each truncated to 0.15s.
    if command -v afplay >/dev/null 2>&1; then
        for snd in Tink Tink Glass Pop; do
            afplay -t 0.15 "/System/Library/Sounds/$snd.aiff" >/dev/null 2>&1
        done
    fi
    if command -v terminal-notifier >/dev/null 2>&1; then
        local args=(-title "$title" -message "$body" -sound Funk
                    -group "orchestrator-$PLAN-task-$TASK_ID")
        [[ -f "$icon" ]] && args+=(-appIcon "$icon" -contentImage "$icon")
        terminal-notifier "${args[@]}" >/dev/null 2>&1 || true
    elif command -v osascript >/dev/null 2>&1; then
        osascript -e "display notification \"$body\" with title \"$title\" sound name \"Funk\"" >/dev/null 2>&1 || true
    fi
}

for f in "$PROMPT" "$ALLOWLIST"; do
    if [[ ! -f "$f" ]]; then
        echo "[task $TASK_ID] missing required file: $f" >&2
        exit 10
    fi
done

if ! shlock -f "$LOCK" -p $$; then
    held_pid="$(head -1 "$LOCK" 2>/dev/null | tr -d ' \t\n')"
    if [[ "${held_pid:-0}" =~ ^[0-9]+$ ]] && [[ "${held_pid:-0}" -gt 0 ]] && ! kill -0 "$held_pid" 2>/dev/null; then
        echo "[task $TASK_ID] stale lock held by dead pid $held_pid; reaping and retrying" >&2
        rm -f "$LOCK"
        if ! shlock -f "$LOCK" -p $$; then
            echo "[task $TASK_ID] lock $LOCK reacquired by another runner — refusing to start" >&2
            exit 30
        fi
    else
        echo "[task $TASK_ID] lock $LOCK already held by live pid ${held_pid:-?} — refusing to start" >&2
        exit 30
    fi
fi
echo "$$" > "$PIDFILE"

# DuckDB state mirror (best-effort). Requires /orchestrator-tooling:install-helpers
# to have symlinked orch-db onto $PATH; otherwise this block is a silent no-op.
db_helper() {
    command -v orch-db >/dev/null 2>&1 || return 0
    orch-db "$@" >/dev/null 2>&1 || true
}
db_helper init
db_helper task-insert "$PLAN" "$TASK_ID" running "$BRANCH" "$WORKTREE"
db_helper task-update "$PLAN" "$TASK_ID" coder_pid="$$" coder_started_at="now()"

cleanup() {
    local code=$?
    pkill -P $$ 2>/dev/null || true
    rm -f "$LOCK" "$PIDFILE"
    if [[ $code -ne 0 ]]; then
        notify_failure "$code"
    fi
    exit $code
}
trap cleanup EXIT
trap 'echo "[task $TASK_ID] caught signal, cleaning up" >&2; exit 130' INT TERM

if [[ -d "$WORKTREE" ]]; then
    git -C "$REPO" worktree remove --force "$WORKTREE" 2>/dev/null || true
    rm -rf "$WORKTREE"
fi
if git -C "$REPO" show-ref --verify --quiet "refs/heads/$BRANCH"; then
    git -C "$REPO" branch -D "$BRANCH" >/dev/null
fi
# Random launch stagger so concurrent wave dispatches don't all hit the
# model provider at t=0 (rate-limit smoothing).
sleep "$(awk -v max="$LAUNCH_STAGGER_SECONDS" 'BEGIN { srand(); printf "%.3f", rand() * max }')"

# Wait-with-timeout on a shared worktree-create lock so concurrent tasks
# can't race the main repo's index. shlock is already a dep and is portable
# on macOS where flock isn't available by default.
WT_DEADLINE=$(( $(date +%s) + 30 ))
while ! shlock -f "$WT_LOCK" -p $$ 2>/dev/null; do
    if [[ $(date +%s) -ge $WT_DEADLINE ]]; then
        echo "[task $TASK_ID] timed out waiting for worktree-create lock" >&2
        exit 20
    fi
    sleep 0.1
done
WT_OK=1
git -C "$REPO" worktree add -b "$BRANCH" "$WORKTREE" "${ORCH_BASE_BRANCH:-main}" >/dev/null 2>&1 || WT_OK=0
rm -f "$WT_LOCK"
if [[ $WT_OK -eq 0 ]]; then
    echo "[task $TASK_ID] failed to create worktree at $WORKTREE on $BRANCH" >&2
    exit 20
fi

echo "[task $TASK_ID] worktree: $WORKTREE branch: $BRANCH log: $CODER_LOG"

PROMPT_BUF="$(mktemp)"
{
    cat <<EOF_HEADER
You are executing ONE task in an isolated git worktree at $WORKTREE on branch $BRANCH (branched from main). Do exactly what the task says. Do not modify files outside the allow-list below. Do not run tasks other than this one. Do not invent extra files (no Placeholder.swift, no scratch files). When the task says a build warning is acceptable, accept it — do not paper over it by writing extra sources.

Allowed file paths (only these may be created or modified — anything else will fail the post-task validator and your work will be discarded):
$(sed 's/^/  - /' "$ALLOWLIST")

End your run by committing exactly once with the exact commit message specified in the task.

=== TASK ===
EOF_HEADER
    cat "$PROMPT"
    cat <<'EOF_FOOTER'

=== END TASK ===

Output policy: terse. Print commands and their key results only. End with "TASK COMPLETE" when finished.
EOF_FOOTER
} > "$PROMPT_BUF"

TASK_RETRIES="${ORCH_TASK_RETRIES:-0}"
ATTEMPT=0
while :; do
    ATTEMPT=$((ATTEMPT + 1))
    set +e
    gtimeout --kill-after=30s "$TIMEOUT_SECONDS" \
        opencode run \
            -m "$CODER_MODEL" \
            --dangerously-skip-permissions \
            --dir "$WORKTREE" \
            "$(cat "$PROMPT_BUF")" \
            > "$CODER_LOG" 2>&1
    CODER_EXIT=$?
    set -e

    if [[ $CODER_EXIT -eq 0 ]]; then
        break
    fi
    if [[ $CODER_EXIT -eq 124 ]]; then
        rm -f "$PROMPT_BUF"
        echo "[task $TASK_ID] TIMEOUT after ${TIMEOUT_SECONDS}s on attempt $ATTEMPT (log: $CODER_LOG)" >&2
        db_helper task-update "$PLAN" "$TASK_ID" state=timeout coder_finished_at="now()" coder_exit_code="$CODER_EXIT"
        exit 40
    fi
    if [[ $TASK_RETRIES -le 0 ]]; then
        rm -f "$PROMPT_BUF"
        echo "[task $TASK_ID] coder exited $CODER_EXIT on attempt $ATTEMPT (log: $CODER_LOG)" >&2
        db_helper task-update "$PLAN" "$TASK_ID" state=failed coder_finished_at="now()" coder_exit_code="$CODER_EXIT"
        exit 50
    fi
    TASK_RETRIES=$((TASK_RETRIES - 1))
    echo "[task $TASK_ID] coder exited $CODER_EXIT on attempt $ATTEMPT; resetting worktree and retrying (${TASK_RETRIES} retries left)" >&2
    # Discard any partial work the failed attempt left behind.
    git -C "$WORKTREE" reset --hard main >/dev/null 2>&1 || true
    git -C "$WORKTREE" clean -fd >/dev/null 2>&1 || true
    sleep 2
done
rm -f "$PROMPT_BUF"

COMMITS_AHEAD="$(git -C "$WORKTREE" rev-list --count "main..$BRANCH")"
if [[ "$COMMITS_AHEAD" -eq 0 ]]; then
    echo "[task $TASK_ID] no commit produced on $BRANCH" >&2
    exit 60
fi

CHANGED_FILES="$(git -C "$WORKTREE" diff --name-only "main..$BRANCH")"
VIOLATIONS=""
while IFS= read -r changed; do
    [[ -z "$changed" ]] && continue
    matched=0
    while IFS= read -r allowed; do
        [[ -z "$allowed" ]] && continue
        [[ "$allowed" =~ ^# ]] && continue
        if [[ "$changed" == "$allowed" || "$changed" == "$allowed"* ]]; then
            matched=1
            break
        fi
    done < "$ALLOWLIST"
    if [[ $matched -eq 0 ]]; then
        VIOLATIONS="$VIOLATIONS$changed"$'\n'
    fi
done <<< "$CHANGED_FILES"

if [[ -n "$VIOLATIONS" ]]; then
    echo "[task $TASK_ID] REJECTED — diff escaped allow-list:" >&2
    echo "$VIOLATIONS" >&2
    echo "[task $TASK_ID] worktree kept for inspection at $WORKTREE" >&2
    db_helper task-update "$PLAN" "$TASK_ID" state=rejected allowlist_ok=false coder_finished_at="now()"
    exit 70
fi
db_helper task-update "$PLAN" "$TASK_ID" state=committed allowlist_ok=true coder_finished_at="now()" coder_exit_code=0

# Usage tracker (best-effort, silent no-op if orch-usage-record missing)
if command -v orch-usage-record >/dev/null 2>&1; then
    orch-usage-record coder "$PLAN" "$TASK_ID" "$CODER_LOG" "$CODER_MODEL" >/dev/null 2>&1 || true
fi

# Queue this finished task for the long-running review-daemon to process
# serially out-of-band. Envelope is minimal: {plan, task_id, ts}. Everything
# else (branch, worktree, spec path) is derived in the daemon from the
# orchestrator's naming convention. Saves ~70% on queue-line size.
REVIEW_QUEUE="$ORCH_RUNTIME/review-queue.jsonl"
REVIEW_ENVELOPE="$(jq -nc \
    --arg plan "$PLAN" \
    --arg task_id "$TASK_ID" \
    --argjson ts "$(date +%s)" \
    '{plan:$plan, task_id:$task_id, ts:$ts}')"
echo "$REVIEW_ENVELOPE" >> "$REVIEW_QUEUE"

echo "[task $TASK_ID] OK  commits=$COMMITS_AHEAD  branch=$BRANCH"
echo "[task $TASK_ID] coder-log=$CODER_LOG"
echo "[task $TASK_ID] queued for review: $REVIEW_QUEUE (start daemon via tools/orchestrator/review-daemon.sh start)"
exit 0
