#!/usr/bin/env bash
# Regression test for v4-hackathon review CRIT-6.
#
# Bug: `dispatch-rotated.sh` populates `PID_TO_TASK` / `PID_TO_MODEL` inside a
# `{ ... } | tee -a "$DISPATCH_LOG"` pipeline. Bash runs each side of a `|` in
# a subshell, so the assignments never reach the parent. The wait loop then
# iterates over an empty map, `wait` is never called, and dispatch exits 0
# even when every task failed.
#
# This test has two parts:
#   1. A minimal repro of the language-level pattern. Asserts the subshell
#      drops the assignment, which is what makes the dispatcher silently
#      succeed.
#   2. A source check against `dispatch-rotated.sh` itself: the wait loop must
#      see a populated map. We fail if the script still contains the broken
#      shape (the `} | tee -a "$DISPATCH_LOG"` directly above the
#      `for pid in "${!PID_TO_TASK[@]}"` line).
#
# Both checks fire today; either fix (`shopt -s lastpipe`, restructure the
# pipeline, or write pids to a file and read them back outside) flips this
# test green.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DISPATCH="$SCRIPT_DIR/../dispatch-rotated.sh"

fail=0

# ── Check 1: minimal language-level repro ───────────────────────────────────
# bash 5+ is required for `declare -A`; the orchestrator already mandates it.
if ! /opt/homebrew/bin/bash --version >/dev/null 2>&1; then
    BASH_BIN="$(command -v bash)"
else
    BASH_BIN="/opt/homebrew/bin/bash"
fi

REPRO_OUT="$("$BASH_BIN" -c '
    declare -A map=()
    { map[a]=1; } | cat >/dev/null
    echo "${map[a]:-EMPTY}"
')"

# The subshell pattern is the bug. If the value is "EMPTY" we are reproducing
# CRIT-6; if it is "1" the bash version or shopt option neutralises the bug
# and dispatch-rotated.sh would behave correctly.
if [[ "$REPRO_OUT" == "EMPTY" ]]; then
    echo "FAIL (CRIT-6 minimal repro): { map[a]=1; } | cat dropped the assignment ($BASH_BIN)"
    echo "       dispatch-rotated.sh's wait loop will iterate over an empty PID_TO_TASK."
    fail=1
else
    echo "OK   (CRIT-6 minimal repro): subshell preserved the assignment ($BASH_BIN says \"$REPRO_OUT\")"
fi

# ── Check 2: source pattern in dispatch-rotated.sh ──────────────────────────
# The broken shape is exactly: the dispatch loop closes with `} | tee ...`
# immediately above the wait loop's `for pid in "${!PID_TO_TASK[@]}"`. If both
# lines exist within a 15-line window, the wait loop will see an empty map.
if [[ ! -f "$DISPATCH" ]]; then
    echo "SKIP (CRIT-6 source check): $DISPATCH not found"
else
    # The wait loop iterates over PID_TO_TASK. Find that line, then walk
    # backwards to the closest `} | tee -a "$DISPATCH_LOG"` — if it sits
    # within ~30 lines above the wait, the dispatch loop's PID assignments
    # were made in a subshell and the wait sees an empty map.
    WAIT_LINE="$(grep -n '^for pid in "\${!PID_TO_TASK\[@\]}"' "$DISPATCH" | head -1 | cut -d: -f1)"
    TEE_LINE=""
    if [[ -n "$WAIT_LINE" ]]; then
        # Largest `} | tee` line number that is strictly less than WAIT_LINE.
        TEE_LINE="$(grep -n '^} | tee -a "\$DISPATCH_LOG"' "$DISPATCH" \
                    | awk -F: -v w="$WAIT_LINE" '$1 < w { print $1 }' \
                    | tail -1)"
    fi
    if [[ -n "$TEE_LINE" && -n "$WAIT_LINE" && $((WAIT_LINE - TEE_LINE)) -le 30 ]]; then
        echo "FAIL (CRIT-6 source check): dispatch-rotated.sh:$TEE_LINE ends the dispatch loop with"
        echo "       '} | tee -a \"\$DISPATCH_LOG\"' and the wait loop at :$WAIT_LINE iterates an"
        echo "       associative array populated inside that pipeline — see review CRIT-6."
        fail=1
    else
        echo "OK   (CRIT-6 source check): dispatch-rotated.sh no longer ties the wait loop to a"
        echo "       subshell-populated associative array."
    fi
fi

exit "$fail"
