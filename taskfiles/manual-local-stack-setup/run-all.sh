#!/usr/bin/env bash
# run-all.sh — invoke every stage script in order, halting on first
# failure unless --keep-going is passed.
#
#   bash taskfiles/manual-local-stack-setup/run-all.sh                 # default
#   bash taskfiles/manual-local-stack-setup/run-all.sh --keep-going    # don't halt
#   bash taskfiles/manual-local-stack-setup/run-all.sh --only 03,05    # subset
#
# Numbered stage files: 00 → 12. Appendix B scripts (B.2 only today;
# B.1 / B.3-B.6 still inline in MANUAL_TEST_CHECKLIST.md as the user
# scripts those off the host shell history) are run separately.

set -uo pipefail

KEEP_GOING=0
ONLY=""
for arg in "$@"; do
    case "$arg" in
        --keep-going|-k) KEEP_GOING=1 ;;
        --only=*)        ONLY="${arg#--only=}" ;;
        --only)          shift; ONLY="${1:-}" ;;
        --help|-h)       sed -n '1,15p' "${BASH_SOURCE[0]:-$0}"; exit 0 ;;
    esac
done

DIR="$(dirname "$0")"
total_pass=0; total_fail=0; halted=0

for f in "$DIR"/[0-9][0-9]-*.sh; do
    [[ -f "$f" ]] || continue
    base="$(basename "$f")"
    num="${base%%-*}"
    if [[ -n "$ONLY" ]] && ! echo ",$ONLY," | grep -q ",$num,"; then
        continue
    fi
    bash "$f"
    rc=$?
    if [[ $rc -ne 0 ]]; then
        total_fail=$((total_fail + 1))
        if [[ $KEEP_GOING -eq 0 ]]; then
            printf '\n  %s failed (rc=%d). Pass --keep-going to ignore.\n' "$base" "$rc"
            halted=1
            break
        fi
    else
        total_pass=$((total_pass + 1))
    fi
done

printf '\n========\n  total: %d stages passed, %d failed (halted=%s)\n========\n' \
    "$total_pass" "$total_fail" "$halted"

[[ $total_fail -eq 0 ]] && exit 0 || exit 1
