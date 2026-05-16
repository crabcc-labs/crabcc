#!/usr/bin/env bash
# Regression test for v4-hackathon review CRIT-7.
#
# Bug: `run-task.sh`'s retry path hardcodes
#     git -C "$WORKTREE" reset --hard main
# while every other reset uses `${ORCH_BASE_BRANCH:-main}` (see line ~190 and
# the `BASE_REF` assignment at the post-retry block). When a wave is
# dispatched from `v4-hackathon` with `ORCH_BASE_BRANCH=v4-hackathon`, a
# retry rebases the model onto the wrong ancestor; the cherry-pick then either
# produces an incorrect commit or fails the allow-list check.
#
# Commit `d5a77606` claimed to make run-task.sh "honor ORCH_BASE_BRANCH" but
# the retry branch was missed. This test pins the contract via a source grep:
# if any `git ... reset --hard main` survives in run-task.sh, fail.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUN_TASK="$SCRIPT_DIR/../run-task.sh"

if [[ ! -f "$RUN_TASK" ]]; then
    echo "FAIL: $RUN_TASK not found"
    exit 1
fi

# A literal `reset --hard main` (with optional surrounding whitespace and an
# arg list, but `main` not followed by an alphanumeric — so `mainline-branch`
# or similar is not matched). The single allowed form is
# `reset --hard "${ORCH_BASE_BRANCH:-main}"` or `reset --hard "$BASE_REF"`.
HITS="$(grep -nE 'reset --hard[[:space:]]+main([^A-Za-z0-9._/-]|$)' "$RUN_TASK" || true)"

if [[ -n "$HITS" ]]; then
    echo "FAIL (CRIT-7): run-task.sh hardcodes 'reset --hard main' on at least one branch:"
    echo "$HITS" | sed 's/^/       /'
    echo "       Replace with: git -C \"\$WORKTREE\" reset --hard \"\${ORCH_BASE_BRANCH:-main}\""
    exit 1
fi

# Also assert the retry section actually uses ORCH_BASE_BRANCH (defensive —
# catches the case where someone deletes the reset entirely without porting
# it).
if ! grep -qE 'reset --hard "\$\{ORCH_BASE_BRANCH:-main\}"|reset --hard "\$BASE_REF"' "$RUN_TASK"; then
    echo "FAIL (CRIT-7): run-task.sh no longer contains a base-branch-aware reset."
    echo "       Expected at least one: git ... reset --hard \"\${ORCH_BASE_BRANCH:-main}\""
    exit 1
fi

echo "OK   (CRIT-7): run-task.sh retry path respects ORCH_BASE_BRANCH."
exit 0
