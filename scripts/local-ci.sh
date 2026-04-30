#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/local-ci.sh
#
# Canonical "skip GitHub CI" runner. Mirrors what `.github/workflows/ci.yml`
# would do plus extras `task prep-pr` already covers, with a single
# pass/fail summary table at the end so you can paste it into a PR
# description or a release ticket.
#
# Steps (in order):
#   1. fmt-check       — `cargo fmt --all -- --check`
#   2. clippy          — `cargo clippy --workspace --all-targets -- -D warnings`
#   3. test            — `cargo nextest run --workspace --profile ci` if
#                         nextest is installed, else `cargo test --workspace
#                         --no-fail-fast`. Single-threaded for tantivy
#                         determinism (see `fts::rebuild_is_idempotent`).
#   4. doc-build       — `RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps`
#   5. smoke           — index + sym + callers against an ephemeral fixture
#                         (matches the GH ci.yml smoke step).
#   6. memory-smoke    — `task memory-smoke` (CLI memory subcommand round-trip).
#   7. release-build   — `cargo build --release` (only with --release).
#
# Usage:
#   scripts/local-ci.sh                # full local CI (no release build)
#   scripts/local-ci.sh --release      # also build the release binary
#   scripts/local-ci.sh --quick        # skip slow steps (smoke, doc-build)
#   scripts/local-ci.sh --strict       # tantivy / parallel checks too
#   scripts/local-ci.sh --no-clippy    # skip clippy (rare; for CI debugging)
#   scripts/local-ci.sh --keep-output  # don't truncate per-step logs
#   scripts/local-ci.sh -h | --help
#
# Per-step logs land under `.summary/local-ci/<step>.log`. The summary
# table is appended to `.summary/local-ci/SUMMARY.txt` so `task ci-history`
# can read the last few runs.
#
# Exit:
#   0   every selected step passed
#   1   at least one selected step failed
#   2   bad usage / missing toolchain
#
# ---------------------------------------------------------------------------

set -uo pipefail

# ---- args -----------------------------------------------------------------

DO_RELEASE=0
DO_QUICK=0
DO_STRICT=0
DO_CLIPPY=1
KEEP_OUTPUT=0
for arg in "$@"; do
    case "$arg" in
        --release)     DO_RELEASE=1 ;;
        --quick)       DO_QUICK=1 ;;
        --strict)      DO_STRICT=1 ;;
        --no-clippy)   DO_CLIPPY=0 ;;
        --keep-output) KEEP_OUTPUT=1 ;;
        -h|--help)
            sed -n '3,40p' "$0" | sed 's/^# \?//'
            exit 0
            ;;
        *)
            echo "unknown flag: $arg (try --help)" >&2
            exit 2
            ;;
    esac
done

# ---- env ------------------------------------------------------------------

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

OUT_DIR=".summary/local-ci"
mkdir -p "$OUT_DIR"

if [ "$KEEP_OUTPUT" -eq 0 ]; then
    rm -f "$OUT_DIR"/*.log
fi

SUMMARY="$OUT_DIR/SUMMARY.txt"

# Colors. NO_COLOR / non-tty disables.
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    GREEN=$'\033[32m'; RED=$'\033[31m'; DIM=$'\033[2m'; BOLD=$'\033[1m'; RESET=$'\033[0m'
else
    GREEN=""; RED=""; DIM=""; BOLD=""; RESET=""
fi

# Use `cargo +stable` if a workspace toolchain pin doesn't already exist.
# Skip when CARGO is set externally (CI may pass a specific toolchain).
CARGO="${CARGO:-cargo}"
if ! command -v "$CARGO" >/dev/null 2>&1; then
    echo "${RED}error: $CARGO not found on PATH${RESET}" >&2
    exit 2
fi

# Decide nextest vs cargo test up front so the summary mentions which
# runner produced the result.
TEST_RUNNER="cargo test"
if command -v cargo-nextest >/dev/null 2>&1; then
    TEST_RUNNER="cargo nextest"
fi

# ---- step harness ---------------------------------------------------------

declare -a STEP_NAMES=()
declare -a STEP_STATUS=()
declare -a STEP_TIME=()
declare -a STEP_NOTE=()

run_step() {
    local name="$1"
    local note="${2:-}"
    shift 2
    local log="$OUT_DIR/$name.log"

    printf '%s%s%s ... ' "$BOLD" "$name" "$RESET"

    local t0 t1 elapsed
    t0=$(date +%s)

    if "$@" >"$log" 2>&1; then
        t1=$(date +%s)
        elapsed=$((t1 - t0))
        printf '%sok%s %s(%ss)%s\n' "$GREEN" "$RESET" "$DIM" "$elapsed" "$RESET"
        STEP_NAMES+=("$name")
        STEP_STATUS+=("ok")
        STEP_TIME+=("$elapsed")
        STEP_NOTE+=("$note")
    else
        t1=$(date +%s)
        elapsed=$((t1 - t0))
        printf '%sFAIL%s %s(%ss; see %s)%s\n' "$RED" "$RESET" "$DIM" "$elapsed" "$log" "$RESET"
        STEP_NAMES+=("$name")
        STEP_STATUS+=("FAIL")
        STEP_TIME+=("$elapsed")
        STEP_NOTE+=("$note  log=$log")
    fi
}

# ---- steps ----------------------------------------------------------------

echo "${BOLD}crabcc local CI${RESET} ${DIM}(root=$ROOT, runner=$TEST_RUNNER)${RESET}"
echo

run_step fmt-check "" "$CARGO" fmt --all -- --check

if [ "$DO_CLIPPY" -eq 1 ]; then
    # Match `task lint` — no `--all-features` because `simd-cosine`
    # gates on nightly-only `portable_simd` and stable-channel clippy
    # errors out with E0554 if asked to enable it.
    run_step clippy "" \
        "$CARGO" clippy --workspace --all-targets -- -D warnings
fi

# Tests. Single-threaded by default (tantivy contention) unless --strict.
if [ "$DO_STRICT" -eq 1 ]; then
    TEST_ARGS=()
else
    TEST_ARGS=(-- --test-threads=1)
fi

if [ "$TEST_RUNNER" = "cargo nextest" ]; then
    # nextest reads `--test-threads` differently — use `--test-threads`
    # directly on the nextest CLI.
    if [ "$DO_STRICT" -eq 1 ]; then
        run_step test "$TEST_RUNNER" "$CARGO" nextest run --workspace
    else
        run_step test "$TEST_RUNNER" "$CARGO" nextest run --workspace --test-threads=1
    fi
else
    run_step test "$TEST_RUNNER" "$CARGO" test --workspace --no-fail-fast "${TEST_ARGS[@]}"
fi

if [ "$DO_QUICK" -eq 0 ]; then
    run_step doc-build "RUSTDOCFLAGS=-D warnings" \
        env RUSTDOCFLAGS="-D warnings" "$CARGO" doc --workspace --no-deps
fi

# Smoke E2E — uses the release binary if it's already built, else debug.
# The CI workflow uses release; we mirror that.
if [ "$DO_QUICK" -eq 0 ] || [ "$DO_RELEASE" -eq 1 ]; then
    run_step smoke "ts fixture: index → sym → callers" \
        bash -c '
            set -euo pipefail
            FIX=$(mktemp -d)
            cleanup() { rm -rf "$FIX"; }
            trap cleanup EXIT
            cd "$FIX"
            cat > a.ts <<EOF
export function hello(name: string) { return name; }
hello("world");
EOF
            BIN="'$CARGO' run --quiet --release -p crabcc-cli --"
            $BIN index >/dev/null
            $BIN sym hello | grep -q "\"name\":\"hello\""
            $BIN callers hello --count | grep -q "\"count\""
        '
fi

if [ -f Taskfile.yml ] && command -v task >/dev/null 2>&1; then
    run_step memory-smoke "task memory-smoke" task memory-smoke
fi

if [ "$DO_RELEASE" -eq 1 ]; then
    run_step release-build "cargo build --release" \
        "$CARGO" build --release --workspace
fi

# ---- summary --------------------------------------------------------------

echo
echo "${BOLD}Summary${RESET}"
fail_count=0
{
    printf '\n=== local-ci %s ===\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    for i in "${!STEP_NAMES[@]}"; do
        printf '  %-14s %-4s  %3ss  %s\n' \
            "${STEP_NAMES[$i]}" "${STEP_STATUS[$i]}" "${STEP_TIME[$i]}" "${STEP_NOTE[$i]}"
    done
} | tee -a "$SUMMARY"

for s in "${STEP_STATUS[@]}"; do
    [ "$s" != "ok" ] && fail_count=$((fail_count + 1))
done

echo
if [ "$fail_count" -eq 0 ]; then
    echo "${GREEN}All ${#STEP_STATUS[@]} steps passed.${RESET} Logs: $OUT_DIR"
    exit 0
else
    echo "${RED}${fail_count} step(s) failed.${RESET} See $OUT_DIR for per-step logs."
    exit 1
fi
