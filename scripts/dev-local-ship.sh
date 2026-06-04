#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/dev-local-ship.sh
#
# Autofix → commit → local CI → structured report → push + PR.
# Called by `task dev:local:ship`. The five stages always run in order;
# any failure prints a structured report and exits non-zero so the caller
# knows exactly which step broke.
#
# Usage:
#   scripts/dev-local-ship.sh --msg "feat(cli): add --json flag" [options]
#
# Options:
#   --msg "..."     commit message (required; Conventional Commits format)
#   --full          run full local-ci instead of --quick
#   --push-only     skip CI, commit+push only (useful after fixing CI errors)
#   --base <ref>    PR base branch (default: main)
#   --no-autofix    skip cargo fmt autofix
#   --dry-run       print what would happen, don't execute
#
# Bypass hooks on push: pass CRABCC_SKIP_HOOKS=1 in the caller's env.
#
# Output:
#   .summary/dev-local-ship.log   full log of every step
#   .summary/dev-local-ship.json  machine-readable step results
#
# Exit:
#   0   all steps passed, PR created or already open
#   1   one or more steps failed (see report)
#   2   bad usage
# ---------------------------------------------------------------------------

set -uo pipefail

# ── args ────────────────────────────────────────────────────────────────────
MSG=""
FULL=0
PUSH_ONLY=0
BASE="main"
NO_AUTOFIX=0
DRY_RUN=0

while [ $# -gt 0 ]; do
    case "$1" in
        --msg)       shift; MSG="${1:-}" ;;
        --full)      FULL=1 ;;
        --push-only) PUSH_ONLY=1 ;;
        --base)      shift; BASE="${1:-main}" ;;
        --no-autofix) NO_AUTOFIX=1 ;;
        --dry-run)   DRY_RUN=1 ;;
        -h|--help)
            sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "dev-local-ship: unknown arg '$1' (try --help)" >&2; exit 2 ;;
    esac
    shift
done

# Allow Taskfile to pass multi-word strings via env vars (avoids word-splitting).
[ -n "${_SHIP_MSG:-}"  ] && MSG="$_SHIP_MSG"
[ -n "${_SHIP_BASE:-}" ] && BASE="$_SHIP_BASE"
[ "${_SHIP_FULL:-}"       = "1" ] && FULL=1
[ "${_SHIP_PUSH_ONLY:-}"  = "1" ] && PUSH_ONLY=1
[ "${_SHIP_NO_AUTOFIX:-}" = "1" ] && NO_AUTOFIX=1
[ "${_SHIP_DRY_RUN:-}"    = "1" ] && DRY_RUN=1

[ -n "$MSG" ] || {
    echo "dev-local-ship: --msg '...' is required (Conventional Commits format)" >&2
    echo "  example: --msg 'feat(cli): add --json flag'" >&2
    exit 2
}

# ── helpers ─────────────────────────────────────────────────────────────────
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "dev-local-ship: not in a git repo" >&2; exit 2
}
cd "$REPO_ROOT"

OUT_DIR=".summary"
mkdir -p "$OUT_DIR"

LOG="$OUT_DIR/dev-local-ship.log"
JSON="$OUT_DIR/dev-local-ship.json"
: >"$LOG"

TS="$(date -u +%Y-%m-%dT%H:%MZ)"

# Color helpers — gracefully degrade when not on a TTY.
if [ -t 1 ] && command -v tput >/dev/null 2>&1; then
    BOLD="$(tput bold 2>/dev/null || true)"
    GREEN="$(tput setaf 2 2>/dev/null || true)"
    RED="$(tput setaf 1 2>/dev/null || true)"
    YELLOW="$(tput setaf 3 2>/dev/null || true)"
    DIM="$(tput dim 2>/dev/null || true)"
    RESET="$(tput sgr0 2>/dev/null || true)"
else
    BOLD="" GREEN="" RED="" YELLOW="" DIM="" RESET=""
fi

BAR="${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"

STEP_TOTAL=5
declare -A STEP_STATUS  # "ok" | "fail" | "skip"
declare -A STEP_DETAIL

step_ok()   { STEP_STATUS[$1]="ok";   STEP_DETAIL[$1]="${2:-}"; }
step_fail() { STEP_STATUS[$1]="fail"; STEP_DETAIL[$1]="${2:-}"; }
step_skip() { STEP_STATUS[$1]="skip"; STEP_DETAIL[$1]="${2:-}"; }

print_report() {
    local exit_code="${1:-0}"
    echo ""
    echo "${BAR}"
    printf "${BOLD}%-4s dev:local:ship${RESET}%s${DIM}%s${RESET}\n" "" "" "  $TS"
    echo "${BAR}"
    for i in 1 2 3 4 5; do
        local label detail status icon color
        case $i in
            1) label="autofix   " ;;
            2) label="commit    " ;;
            3) label="local CI  " ;;
            4) label="push      " ;;
            5) label="PR        " ;;
        esac
        status="${STEP_STATUS[$i]:-skip}"
        detail="${STEP_DETAIL[$i]:-}"
        case "$status" in
            ok)   icon="✓"; color="$GREEN" ;;
            fail) icon="✗"; color="$RED"   ;;
            skip) icon="–"; color="$DIM"   ;;
        esac
        printf " %d/%d  %s${color}%s${RESET}  %s\n" \
            "$i" "$STEP_TOTAL" "$label" "$icon" "$detail"
    done
    echo "${BAR}"

    if [ "$exit_code" = "0" ]; then
        printf " ${GREEN}${BOLD}PASSED${RESET}\n"
    else
        printf " ${RED}${BOLD}FAILED${RESET} at step %s\n" "${FAILED_STEP:-?}"
        echo ""
        printf " Fix above, then re-run:\n"
        if [ "${STEP_STATUS[3]:-}" = "fail" ]; then
            printf "   task dev:local:ship MSG=\"%s\" PUSH_ONLY=1\n" "$MSG"
        else
            printf "   task dev:local:ship MSG=\"%s\"\n" "$MSG"
        fi
        printf " Or reset commit:  git reset HEAD~1\n"
    fi
    echo "${BAR}"
    echo ""

    # Emit machine-readable JSON.
    local s1="${STEP_STATUS[1]:-skip}" s2="${STEP_STATUS[2]:-skip}"
    local s3="${STEP_STATUS[3]:-skip}" s4="${STEP_STATUS[4]:-skip}"
    local s5="${STEP_STATUS[5]:-skip}"
    cat >"$JSON" <<ENDJSON
{
  "ts": "$TS",
  "msg": $(printf '%s' "$MSG" | jq -Rs .),
  "steps": {
    "autofix":  {"status": "$s1", "detail": $(printf '%s' "${STEP_DETAIL[1]:-}" | jq -Rs .)},
    "commit":   {"status": "$s2", "detail": $(printf '%s' "${STEP_DETAIL[2]:-}" | jq -Rs .)},
    "local_ci": {"status": "$s3", "detail": $(printf '%s' "${STEP_DETAIL[3]:-}" | jq -Rs .)},
    "push":     {"status": "$s4", "detail": $(printf '%s' "${STEP_DETAIL[4]:-}" | jq -Rs .)},
    "pr":       {"status": "$s5", "detail": $(printf '%s' "${STEP_DETAIL[5]:-}" | jq -Rs .)}
  },
  "exit": $exit_code
}
ENDJSON
}

run() {
    if [ "$DRY_RUN" = "1" ]; then
        echo "  [dry-run] $*" | tee -a "$LOG"
        return 0
    fi
    echo "  + $*" | tee -a "$LOG"
    "$@" 2>&1 | tee -a "$LOG"
}

FAILED_STEP=""

# ── step 1: autofix (cargo fmt) ─────────────────────────────────────────────
printf "${BOLD}[dev:local:ship]${RESET} 1/%d  autofix\n" "$STEP_TOTAL"
if [ "$NO_AUTOFIX" = "1" ]; then
    step_skip 1 "skipped (--no-autofix)"
else
    if run cargo fmt --all; then
        FMT_CHANGED="$(git diff --name-only --diff-filter=M -- '*.rs' 2>/dev/null || true)"
        if [ -n "$FMT_CHANGED" ]; then
            COUNT="$(echo "$FMT_CHANGED" | wc -l | tr -d ' ')"
            echo "$FMT_CHANGED" | xargs git add
            step_ok 1 "cargo fmt  ($COUNT file(s) rewritten + restaged)"
        else
            step_ok 1 "cargo fmt  (no changes)"
        fi
    else
        FAILED_STEP=1
        step_fail 1 "cargo fmt failed — see $LOG"
        step_skip 2 ""; step_skip 3 ""; step_skip 4 ""; step_skip 5 ""
        print_report 1; exit 1
    fi
fi

# ── step 2: commit ───────────────────────────────────────────────────────────
printf "${BOLD}[dev:local:ship]${RESET} 2/%d  commit\n" "$STEP_TOTAL"
# Stage all tracked changes (not untracked — author's responsibility).
git add -u 2>/dev/null || true

STAGED="$(git diff --cached --name-only 2>/dev/null || true)"
if [ -z "$STAGED" ]; then
    FAILED_STEP=2
    step_fail 2 "nothing staged — working tree clean or only untracked files"
    step_skip 3 ""; step_skip 4 ""; step_skip 5 ""
    print_report 1
    echo "  tip: stage new files first with  git add <file>  before running ship" >&2
    exit 1
fi
N_STAGED="$(echo "$STAGED" | wc -l | tr -d ' ')"

if [ "$DRY_RUN" = "1" ]; then
    echo "  [dry-run] git commit -m \"$MSG\"  ($N_STAGED files)"
    step_ok 2 "dry-run  ($N_STAGED files staged)"
elif git commit -m "$MSG" >> "$LOG" 2>&1; then
    SHA="$(git rev-parse --short HEAD)"
    step_ok 2 "$SHA  ($N_STAGED files)"
else
    FAILED_STEP=2
    step_fail 2 "git commit failed — hooks rejected or commit failed (see $LOG)"
    step_skip 3 ""; step_skip 4 ""; step_skip 5 ""
    print_report 1; exit 1
fi

# ── step 3: local CI ─────────────────────────────────────────────────────────
printf "${BOLD}[dev:local:ship]${RESET} 3/%d  local CI\n" "$STEP_TOTAL"
if [ "$PUSH_ONLY" = "1" ]; then
    step_skip 3 "skipped (--push-only)"
else
    CI_FLAGS="--quick"
    CI_LABEL="quick"
    if [ "$FULL" = "1" ]; then CI_FLAGS=""; CI_LABEL="full"; fi

    if [ "$DRY_RUN" = "1" ]; then
        echo "  [dry-run] scripts/local-ci.sh $CI_FLAGS"
        step_ok 3 "dry-run ($CI_LABEL)"
    elif bash scripts/local-ci.sh $CI_FLAGS >> "$LOG" 2>&1; then
        step_ok 3 "$CI_LABEL  (log: $LOG)"
    else
        FAILED_STEP=3
        CI_DETAIL="$CI_LABEL failed"
        if [ -f ".summary/local-ci/SUMMARY.txt" ]; then
            FIRST_FAIL="$(grep -m1 '✗\|FAIL\|error' .summary/local-ci/SUMMARY.txt 2>/dev/null | head -1 || true)"
            [ -n "$FIRST_FAIL" ] && CI_DETAIL="$CI_LABEL: $FIRST_FAIL"
        fi
        step_fail 3 "$CI_DETAIL  →  $LOG"
        step_skip 4 ""; step_skip 5 ""
        print_report 1; exit 1
    fi
fi

# ── step 4: push ─────────────────────────────────────────────────────────────
printf "${BOLD}[dev:local:ship]${RESET} 4/%d  push\n" "$STEP_TOTAL"
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
# We've already run CI; skip pre-push hook to avoid redundant test re-run.
if [ "$DRY_RUN" = "1" ]; then
    echo "  [dry-run] CRABCC_SKIP_HOOKS=1 git push -u origin $BRANCH"
    step_ok 4 "dry-run  origin/$BRANCH"
elif CRABCC_SKIP_HOOKS=1 git push -u origin "$BRANCH" >> "$LOG" 2>&1; then
    step_ok 4 "origin/$BRANCH"
else
    FAILED_STEP=4
    step_fail 4 "push failed (network? permissions? — see $LOG)"
    step_skip 5 ""
    print_report 1; exit 1
fi

# ── step 5: PR ───────────────────────────────────────────────────────────────
printf "${BOLD}[dev:local:ship]${RESET} 5/%d  PR\n" "$STEP_TOTAL"
if ! command -v gh >/dev/null 2>&1; then
    step_skip 5 "gh CLI not found — create PR manually at github.com"
elif [ "$DRY_RUN" = "1" ]; then
    echo "  [dry-run] gh pr create --base $BASE --title \"$MSG\""
    step_ok 5 "dry-run"
else
    EXISTING="$(gh pr view "$BRANCH" --json number,url \
        --jq '"#\(.number)  \(.url)"' 2>/dev/null || true)"
    if [ -n "$EXISTING" ]; then
        step_ok 5 "already open  $EXISTING"
    else
        bash scripts/gen-summary.sh --quiet >> "$LOG" 2>&1 || true
        BODY_ARG=""
        [ -f ".summary/gen-summary.md" ] && BODY_ARG="--body-file .summary/gen-summary.md"
        # Capture stdout only; send stderr to log so error messages don't
        # corrupt the URL. Extract the https URL explicitly rather than
        # relying on tail -1 which breaks if gh emits warnings.
        # shellcheck disable=SC2086
        PR_OUT="$(gh pr create \
            --base "$BASE" \
            --title "$MSG" \
            $BODY_ARG 2>>"$LOG" || true)"
        PR_URL="$(echo "$PR_OUT" | grep -oE 'https://github\.com/[^[:space:]]+' | tail -1 || true)"
        if [ -n "$PR_URL" ]; then
            step_ok 5 "$PR_URL"
        else
            step_fail 5 "gh pr create failed — see $LOG"
            print_report 1; exit 1
        fi
    fi
fi

print_report 0
