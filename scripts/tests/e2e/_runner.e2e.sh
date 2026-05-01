#!/usr/bin/env bash
# scripts/tests/e2e/_runner.e2e.sh — shared helpers for the crabcc e2e suites.
#
# Sourced (not executed) by every suite under scripts/tests/e2e/*.sh.
# Provides:
#   e2e::preflight       check curl/jq/lsof/binary up-front, print env header
#   e2e::ensure_server   start crabcc serve if no server on :7878
#   e2e::http            curl wrapper, sets E2E_HTTP_STATUS / E2E_HTTP_BODY
#   e2e::assert_*        equality / status / jq / substring assertions
#   e2e::run_case        wrap a case body with timing + failure tracking
#   e2e::summary         end-of-run tally; non-zero exit on any failure
#
# Per-run audit trail:
#   ${TMPDIR}/crabcc-e2e-<suite>-<ts>.log   one line per HTTP call + body
#   ${TMPDIR}/crabcc-e2e-<suite>-<ts>.report  pass/fail summary
#
# Style: pure bash, no heredocs of code, no external deps beyond
# curl + jq + standard coreutils. Matches scripts/tests/test-bootstrap.sh.

set -euo pipefail

# ---- repo + binary discovery -------------------------------------------------

E2E_REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
E2E_BIN="$E2E_REPO_ROOT/target/release/crabcc"
E2E_PORT="${E2E_PORT:-7878}"
E2E_BASE="http://127.0.0.1:${E2E_PORT}"
E2E_TMP="${TMPDIR:-/tmp}"
E2E_RESP_FILE="$E2E_TMP/crabcc-e2e-resp.bin"
E2E_HEADERS_FILE="$E2E_TMP/crabcc-e2e-resp.headers"
E2E_SERVE_PID_FILE="$E2E_TMP/crabcc-e2e-serve.pid"
E2E_SERVE_LOG="$E2E_TMP/crabcc-e2e-serve.log"

# Per-run audit trail. Suites set E2E_SUITE_NAME before sourcing this file
# (or the basename fallback below picks it up). Timestamp is fixed at
# source-time so all writes from one suite-run land in one file.
E2E_SUITE_NAME="${E2E_SUITE_NAME:-$(basename "${BASH_SOURCE[1]:-suite}" .sh)}"
E2E_RUN_TS="$(date +%Y%m%d-%H%M%S)"
E2E_RUN_LOG="$E2E_TMP/crabcc-e2e-${E2E_SUITE_NAME}-${E2E_RUN_TS}.log"
E2E_REPORT_FILE="$E2E_TMP/crabcc-e2e-${E2E_SUITE_NAME}-${E2E_RUN_TS}.report"

# Colour helpers (degrade gracefully when not a tty).
if [[ -t 1 ]]; then
    E2E_GRN=$'\033[1;32m'; E2E_RED=$'\033[1;31m'
    E2E_DIM=$'\033[2m';    E2E_YEL=$'\033[1;33m'
    E2E_BLD=$'\033[1m';    E2E_OFF=$'\033[0m'
else
    E2E_GRN=""; E2E_RED=""; E2E_DIM=""; E2E_YEL=""; E2E_BLD=""; E2E_OFF=""
fi

# Counters (per suite).
E2E_PASS_COUNT=0
E2E_FAIL_COUNT=0
E2E_HTTP_CALL_COUNT=0
declare -a E2E_FAIL_NAMES=()

# Globals populated by e2e::http.
E2E_HTTP_STATUS=0
E2E_HTTP_BODY="$E2E_RESP_FILE"
E2E_HTTP_LAST_URL=""
E2E_HTTP_LAST_METHOD=""

# ---- internal logging --------------------------------------------------------

# Append a structured entry to the run log. Every preflight check, HTTP call,
# server start, and assertion failure feeds in here so the file is the
# single source of truth for "what did the run actually do".
e2e::_log() {
    printf '[%s] %s\n' "$(date '+%H:%M:%S')" "$*" >> "$E2E_RUN_LOG"
}

e2e::_log_blob() {
    # _log_blob <label> <file>
    local label="$1"; local file="$2"
    if [[ -s "$file" ]]; then
        {
            printf '%s ----- %s -----\n' "$(date '+%H:%M:%S')" "$label"
            head -c 8192 "$file"
            printf '\n%s ----- /%s -----\n' "$(date '+%H:%M:%S')" "$label"
        } >> "$E2E_RUN_LOG"
    fi
}

# ---- preflight ---------------------------------------------------------------

# Check every external dependency up-front. Returns 0 on success, exits 2
# with a clear message on failure. Always prints a 1-line "env" banner that
# names the suite, log file, and binary so the user can grep the output.
e2e::preflight() {
    : > "$E2E_RUN_LOG"
    : > "$E2E_REPORT_FILE"
    e2e::_log "preflight: suite=$E2E_SUITE_NAME run=$E2E_RUN_TS"
    e2e::_log "repo_root=$E2E_REPO_ROOT"
    e2e::_log "base=$E2E_BASE"

    local missing=()
    for tool in curl jq awk grep sed; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            missing+=("$tool")
        fi
    done
    # `lsof` is optional (only used to detect a port squatter) — warn,
    # don't fail.
    if ! command -v lsof >/dev/null 2>&1; then
        e2e::_log "preflight: lsof missing — skipping port-squatter detection"
    fi

    if (( ${#missing[@]} > 0 )); then
        printf '%s[e2e] missing required tools:%s %s\n' \
            "$E2E_RED" "$E2E_OFF" "${missing[*]}" >&2
        printf '       install with: brew install %s\n' "${missing[*]}" >&2
        e2e::_log "preflight: FAIL — missing tools: ${missing[*]}"
        exit 2
    fi

    if [[ "${E2E_REQUIRE_BIN:-0}" == "1" && ! -x "$E2E_BIN" ]]; then
        printf '%s[e2e] missing release binary: %s%s\n' \
            "$E2E_RED" "$E2E_BIN" "$E2E_OFF" >&2
        printf '       run: cargo build --release\n' >&2
        e2e::_log "preflight: FAIL — missing binary $E2E_BIN"
        exit 2
    fi

    e2e::_log "preflight: OK (curl=$(curl --version | head -1), jq=$(jq --version 2>/dev/null))"
    printf '%s[e2e]%s suite=%s log=%s\n' \
        "$E2E_BLD" "$E2E_OFF" "$E2E_SUITE_NAME" "$E2E_RUN_LOG"
}

e2e::require_jq() { :; }   # back-compat — kept for any external sourcing
e2e::require_bin() {
    E2E_REQUIRE_BIN=1
    if [[ ! -x "$E2E_BIN" ]]; then
        printf '%s[e2e] missing release binary: %s%s\n' "$E2E_RED" "$E2E_BIN" "$E2E_OFF" >&2
        printf '       run: cargo build --release\n' >&2
        exit 2
    fi
}

# ---- server lifecycle --------------------------------------------------------

# Returns 0 if server is healthy on $E2E_PORT.
e2e::_health_check() {
    local code
    code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 2 \
              "$E2E_BASE/api/health" 2>/dev/null || echo 000)"
    [[ "$code" == "200" ]]
}

# Start crabcc serve in the background; record PID so the same process can
# reuse it; leave it running on exit. Don't tear down a server we didn't
# start (detected via lsof on the port at preflight).
e2e::ensure_server() {
    e2e::require_bin

    if e2e::_health_check; then
        printf '%s[e2e] reusing healthy server at %s%s\n' \
            "$E2E_DIM" "$E2E_BASE" "$E2E_OFF"
        e2e::_log "server: reuse $E2E_BASE"
        return 0
    fi

    if command -v lsof >/dev/null 2>&1; then
        local squatter
        squatter="$(lsof -ti:"$E2E_PORT" 2>/dev/null || true)"
        if [[ -n "$squatter" ]]; then
            printf '%s[e2e] port %s in use (PID %s) but /api/health did not return 200.%s\n' \
                "$E2E_RED" "$E2E_PORT" "$squatter" "$E2E_OFF" >&2
            e2e::_log "server: FAIL — port squatter PID=$squatter not healthy"
            exit 2
        fi
    fi

    printf '%s[e2e] starting crabcc serve on :%s ...%s\n' \
        "$E2E_DIM" "$E2E_PORT" "$E2E_OFF"
    : > "$E2E_SERVE_LOG"
    nohup "$E2E_BIN" --root "$E2E_REPO_ROOT" serve \
        --port "$E2E_PORT" --no-open \
        > "$E2E_SERVE_LOG" 2>&1 &
    local pid=$!
    echo "$pid" > "$E2E_SERVE_PID_FILE"
    e2e::_log "server: started pid=$pid log=$E2E_SERVE_LOG"

    local i
    for i in $(seq 1 60); do
        if e2e::_health_check; then
            printf '%s[e2e] server up (pid %s) after %ds%s\n' \
                "$E2E_DIM" "$pid" "$((i / 2))" "$E2E_OFF"
            e2e::_log "server: healthy after ${i} polls"
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            printf '%s[e2e] crabcc serve exited before becoming healthy.%s\n' \
                "$E2E_RED" "$E2E_OFF" >&2
            tail -n 40 "$E2E_SERVE_LOG" >&2 || true
            e2e::_log "server: FAIL — child died"
            e2e::_log_blob "serve.log (last 40 lines)" "$E2E_SERVE_LOG"
            exit 2
        fi
        sleep 0.5
    done

    printf '%s[e2e] timeout waiting for /api/health on %s%s\n' \
        "$E2E_RED" "$E2E_BASE" "$E2E_OFF" >&2
    tail -n 40 "$E2E_SERVE_LOG" >&2 || true
    e2e::_log "server: FAIL — health timeout"
    e2e::_log_blob "serve.log (last 40 lines)" "$E2E_SERVE_LOG"
    exit 2
}

# ---- HTTP wrapper ------------------------------------------------------------

# e2e::http <METHOD> <PATH-or-URL> [body-string]
# Always returns 0 — assertions are the caller's job. Every call appends
# a record to the run log: method, url, status, headers preview, body
# preview (capped at 8 KB).
e2e::http() {
    local method="$1"; local target="$2"; local body="${3:-}"
    local url
    if [[ "$target" == http* ]]; then url="$target"
    else                                url="$E2E_BASE$target"
    fi
    E2E_HTTP_LAST_METHOD="$method"
    E2E_HTTP_LAST_URL="$url"
    : > "$E2E_RESP_FILE"
    : > "$E2E_HEADERS_FILE"

    E2E_HTTP_CALL_COUNT=$(( E2E_HTTP_CALL_COUNT + 1 ))
    e2e::_log "[$E2E_HTTP_CALL_COUNT] >>> $method $url"
    if [[ -n "$body" ]]; then
        printf '%s [%d] >>> body: ' "$(date '+%H:%M:%S')" "$E2E_HTTP_CALL_COUNT" >> "$E2E_RUN_LOG"
        printf '%s\n' "$body" | head -c 4096 >> "$E2E_RUN_LOG"
        printf '\n' >> "$E2E_RUN_LOG"
    fi

    local code
    if [[ "$method" == "GET" ]]; then
        code="$(curl -sS -X GET \
            -D "$E2E_HEADERS_FILE" \
            -o "$E2E_RESP_FILE" \
            -w '%{http_code}' \
            --max-time 30 \
            "$url" 2>/dev/null || echo 000)"
    else
        code="$(curl -sS -X "$method" \
            -H 'Content-Type: application/json' \
            --data "$body" \
            -D "$E2E_HEADERS_FILE" \
            -o "$E2E_RESP_FILE" \
            -w '%{http_code}' \
            --max-time 30 \
            "$url" 2>/dev/null || echo 000)"
    fi
    E2E_HTTP_STATUS="${code:-0}"
    e2e::_log "[$E2E_HTTP_CALL_COUNT] <<< status=$E2E_HTTP_STATUS"
    e2e::_log_blob "[$E2E_HTTP_CALL_COUNT] response (first 8KB)" "$E2E_RESP_FILE"
    return 0
}

e2e::http_header() {
    local name="$1"
    awk -v n="$name" 'BEGIN{IGNORECASE=1} \
        tolower($0) ~ "^"tolower(n)":"{ \
            sub(/^[^:]*:[[:space:]]*/, ""); sub(/\r$/, ""); print; exit }' \
        "$E2E_HEADERS_FILE"
}

# ---- assertions --------------------------------------------------------------

e2e::_fail_at() {
    local msg="$1"
    local frame_file="${BASH_SOURCE[2]:-?}"
    local frame_line="${BASH_LINENO[1]:-?}"
    printf '%s  AT %s:%s: %s%s\n' "$E2E_RED" "$frame_file" "$frame_line" "$msg" "$E2E_OFF" >&2
    e2e::_log "  FAIL at $frame_file:$frame_line — $msg"
    E2E_CASE_FAILED=1
}

e2e::assert_eq() {
    local actual="${1-}"; local expected="${2-}"; local msg="${3:-equality}"
    if [[ "$actual" != "$expected" ]]; then
        e2e::_fail_at "$msg: expected $(printf '%q' "$expected") got $(printf '%q' "$actual")"
        return 1
    fi
    return 0
}

e2e::assert_ne() {
    local actual="${1-}"; local forbidden="${2-}"; local msg="${3:-inequality}"
    if [[ "$actual" == "$forbidden" ]]; then
        e2e::_fail_at "$msg: value should not equal $(printf '%q' "$forbidden")"
        return 1
    fi
    return 0
}

e2e::assert_status() {
    local expected="$1"; local msg="${2:-HTTP status}"
    if [[ "${E2E_HTTP_STATUS:-0}" != "$expected" ]]; then
        e2e::_fail_at "$msg: $E2E_HTTP_LAST_METHOD $E2E_HTTP_LAST_URL → $E2E_HTTP_STATUS (wanted $expected)"
        if [[ -s "$E2E_RESP_FILE" ]]; then
            printf '%s  --- response body (first 600 bytes) ---%s\n' "$E2E_DIM" "$E2E_OFF" >&2
            head -c 600 "$E2E_RESP_FILE" >&2
            printf '\n' >&2
        fi
        return 1
    fi
    return 0
}

e2e::assert_json() {
    local filter="$1"; local expected="$2"; local msg="${3:-jq filter}"
    local actual
    if ! actual="$(jq -r "$filter" "$E2E_RESP_FILE" 2>/dev/null)"; then
        e2e::_fail_at "$msg: jq filter '$filter' failed against $E2E_HTTP_LAST_URL"
        return 1
    fi
    if [[ "$actual" != "$expected" ]]; then
        e2e::_fail_at "$msg: filter '$filter' on $E2E_HTTP_LAST_URL → $(printf '%q' "$actual") (wanted $(printf '%q' "$expected"))"
        return 1
    fi
    return 0
}

e2e::assert_contains() {
    local haystack="${1-}"; local needle="${2-}"; local msg="${3:-substring}"
    if [[ "$haystack" != *"$needle"* ]]; then
        local snippet="$haystack"
        if (( ${#snippet} > 200 )); then snippet="${snippet:0:200}…"; fi
        e2e::_fail_at "$msg: substring $(printf '%q' "$needle") not found in $(printf '%q' "$snippet")"
        return 1
    fi
    return 0
}

e2e::assert_file_contains() {
    local file="$1"; local needle="$2"; local msg="${3:-file substring}"
    if ! grep -q -F -- "$needle" "$file" 2>/dev/null; then
        e2e::_fail_at "$msg: substring $(printf '%q' "$needle") not found in $file"
        return 1
    fi
    return 0
}

# ---- case lifecycle ----------------------------------------------------------

e2e::run_case() {
    local name="$1"; shift
    E2E_CASE_FAILED=0
    e2e::_log "===== case: $name ====="
    local started; started="$(date +%s)"
    if "$@"; then :; fi
    local ended; ended="$(date +%s)"
    local elapsed=$(( ended - started ))
    if (( E2E_CASE_FAILED == 0 )); then
        printf '%sPASS%s  %s::%s  (%ds)\n' "$E2E_GRN" "$E2E_OFF" "$E2E_SUITE_NAME" "$name" "$elapsed"
        e2e::_log "===== PASS $name (${elapsed}s) ====="
        E2E_PASS_COUNT=$(( E2E_PASS_COUNT + 1 ))
    else
        printf '%sFAIL%s  %s::%s  (%ds)\n' "$E2E_RED" "$E2E_OFF" "$E2E_SUITE_NAME" "$name" "$elapsed"
        e2e::_log "===== FAIL $name (${elapsed}s) ====="
        E2E_FAIL_COUNT=$(( E2E_FAIL_COUNT + 1 ))
        E2E_FAIL_NAMES+=("$name")
    fi
}

# Final summary. Writes the report file too so a CI / agent / tail-f can
# pick it up without parsing colour codes out of stdout.
e2e::summary() {
    local total=$(( E2E_PASS_COUNT + E2E_FAIL_COUNT ))
    {
        printf 'suite      : %s\n' "$E2E_SUITE_NAME"
        printf 'run_id     : %s\n' "$E2E_RUN_TS"
        printf 'cases      : %d/%d passed\n' "$E2E_PASS_COUNT" "$total"
        printf 'http_calls : %d\n' "$E2E_HTTP_CALL_COUNT"
        printf 'log        : %s\n' "$E2E_RUN_LOG"
        if (( E2E_FAIL_COUNT > 0 )); then
            printf 'failed     : %s\n' "${E2E_FAIL_NAMES[*]}"
        fi
    } > "$E2E_REPORT_FILE"

    if (( E2E_FAIL_COUNT == 0 )); then
        printf '%s%d/%d cases passed in %s%s  %s(log=%s)%s\n' \
            "$E2E_GRN" "$E2E_PASS_COUNT" "$total" "$E2E_SUITE_NAME" "$E2E_OFF" \
            "$E2E_DIM" "$E2E_RUN_LOG" "$E2E_OFF"
        return 0
    fi
    printf '%s%d/%d cases passed in %s (%d failed: %s)%s  %s(log=%s)%s\n' \
        "$E2E_RED" "$E2E_PASS_COUNT" "$total" "$E2E_SUITE_NAME" \
        "$E2E_FAIL_COUNT" "${E2E_FAIL_NAMES[*]}" "$E2E_OFF" \
        "$E2E_DIM" "$E2E_RUN_LOG" "$E2E_OFF"
    return 1
}
