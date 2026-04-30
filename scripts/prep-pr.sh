#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/prep-pr.sh
#
# Single-call pre-PR gate. Runs every check the CI runs (fmt + clippy +
# test) plus a `cargo doc --no-deps` sanity build to catch rustdoc
# warnings before reviewers see them. Output is teed to
# .summary/prep-pr.txt so you can paste it into the PR body.
#
# Usage:
#   scripts/prep-pr.sh                # full gate
#   scripts/prep-pr.sh --no-doc       # skip the doc-build step
#   scripts/prep-pr.sh --quiet        # only print pass/fail summary
#
# Exit:
#   0  every step passed
#   1  at least one step failed (see .summary/prep-pr.txt for details)
#
# ---------------------------------------------------------------------------
# CHANGELOG
#   v1.0.0 (2026-04-30) — initial cut. Backs `task prep-pr`. Pass-through
#                          to cargo fmt / clippy / test / doc.
# ---------------------------------------------------------------------------

set -uo pipefail

DO_DOC=1
QUIET=0
for arg in "$@"; do
    case "$arg" in
        --no-doc) DO_DOC=0 ;;
        --quiet)  QUIET=1 ;;
        --help|-h)
            sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "unknown arg: $arg" >&2; exit 1 ;;
    esac
done

LOG=".summary/prep-pr.txt"
mkdir -p "$(dirname "$LOG")"

# --- terminal styling -----------------------------------------------------
if [ -t 1 ] && command -v tput >/dev/null 2>&1; then
    BOLD="$(tput bold || true)"; DIM="$(tput dim || true)"
    GREEN="$(tput setaf 2 || true)"; RED="$(tput setaf 1 || true)"
    RESET="$(tput sgr0 || true)"
else
    BOLD=""; DIM=""; GREEN=""; RED=""; RESET=""
fi

# Reset the log so reruns don't accumulate.
: >"$LOG"

step() {
    local name="$1"; shift
    [ "$QUIET" = "0" ] && printf "${BOLD}▶ %s${RESET}\n" "$name"
    {
        printf "\n=== %s ===\n" "$name"
        printf "    cmd: %s\n" "$*"
        printf "\n"
        if "$@"; then
            printf "    [ok]\n"
            return 0
        else
            local rc=$?
            printf "    [FAILED rc=%d]\n" "$rc"
            return "$rc"
        fi
    } >>"$LOG" 2>&1
}

failed=0
step "cargo fmt --check"    cargo fmt --all -- --check         || failed=$((failed + 1))
step "cargo clippy"         cargo clippy --workspace --all-targets -- -D warnings || failed=$((failed + 1))
step "cargo test"           cargo test --workspace --no-fail-fast || failed=$((failed + 1))
if [ "$DO_DOC" = "1" ]; then
    # RUSTDOCFLAGS="-D warnings" promotes broken-link / missing-docs into
    # hard errors, matching the strictness of `clippy -D warnings`.
    step "cargo doc --no-deps" \
        env RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps || failed=$((failed + 1))
fi

# --- summary --------------------------------------------------------------
total=3
[ "$DO_DOC" = "1" ] && total=4
passed=$((total - failed))
echo
if [ "$failed" -eq 0 ]; then
    printf "${GREEN}${BOLD}✓ prep-pr: all %d checks passed${RESET}  (log: %s)\n" "$total" "$LOG"
else
    printf "${RED}${BOLD}✗ prep-pr: %d/%d checks failed${RESET}  (log: %s)\n" "$failed" "$total" "$LOG"
    printf "${DIM}re-run individual steps to see full output, or open the log${RESET}\n"
    exit 1
fi
