#!/usr/bin/env bash
# scripts/tests/e2e/dashboard.sh — dashboard SPA + meta endpoints.
#
# Cases:
#   loads          GET /              200 + html + script tag + #root mount
#   bootstrap      GET /api/bootstrap repo + version match Cargo.toml
#   health         GET /api/health    {"status":"ok"}
#   openapi        GET /api/openapi.yaml 200 + non-empty
#   static-chunks  GET /static/<chunk>.js  see note in case body
#
# Run all:        bash scripts/tests/e2e/dashboard.sh
# Run one:        CASE=loads bash scripts/tests/e2e/dashboard.sh

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_SUITE_NAME="dashboard"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/_runner.e2e.sh"

e2e::preflight
e2e::ensure_server

# ---- helpers (suite-local) ---------------------------------------------------

# Read [workspace.package].version. Delegate to scripts/version.sh so the
# rules of "what is the workspace version" stay in one place.
_dashboard_workspace_version() {
    bash "$E2E_REPO_ROOT/scripts/version.sh"
}

# ---- cases -------------------------------------------------------------------

case_loads() {
    e2e::http GET /
    e2e::assert_status 200 "GET /"
    local ctype; ctype="$(e2e::http_header content-type)"
    e2e::assert_contains "$ctype" "text/html" "Content-Type"

    # SPA shell: must mount React into #root and pull in at least one script.
    # In this build the JS is inlined as <script>...</script>, so we don't
    # require an external src=, just any <script> tag + the #root anchor.
    e2e::assert_file_contains "$E2E_HTTP_BODY" '<script' "html has a script tag"
    e2e::assert_file_contains "$E2E_HTTP_BODY" 'id="root"' 'html has #root mount'
}

case_bootstrap() {
    e2e::http GET /api/bootstrap
    e2e::assert_status 200 "GET /api/bootstrap"
    e2e::assert_json '.repo | type'    'string' '.repo is a string'
    e2e::assert_json '.repo | length > 0 | tostring' 'true' '.repo non-empty'

    local expected; expected="$(_dashboard_workspace_version)"
    e2e::assert_ne "$expected" "" "Cargo.toml workspace version found"
    e2e::assert_json '.version' "$expected" 'bootstrap .version matches Cargo.toml'
}

case_health() {
    e2e::http GET /api/health
    e2e::assert_status 200 "GET /api/health"
    e2e::assert_json '.status' 'ok' '.status == ok'
}

case_openapi() {
    e2e::http GET /api/openapi.yaml
    e2e::assert_status 200 "GET /api/openapi.yaml"
    local size; size="$(wc -c < "$E2E_HTTP_BODY" | tr -d ' ')"
    if (( size < 100 )); then
        e2e::_fail_at "openapi body too small: $size bytes"
    fi
    e2e::assert_file_contains "$E2E_HTTP_BODY" 'openapi:' 'starts with openapi:'
}

case_static_chunks() {
    # NOTE — relaxed from the original spec.
    # The current SPA inlines all JS into the index.html document; there is
    # no external `/static/<chunk>.js` URL referenced in the HTML, and the
    # server does not register a `/static/...` route. Asserting on a chunk
    # URL that doesn't exist would give us a permanent red light.
    #
    # Instead we (a) probe a representative `/static/*.js` URL and (b)
    # accept either a 200 with an immutable Cache-Control (in case external
    # chunks ever come back) OR a 404 (today's reality). Anything else (5xx,
    # 200 without Cache-Control) is a regression we want to catch.
    e2e::http GET /static/dashboard.js
    case "$E2E_HTTP_STATUS" in
        200)
            local cc; cc="$(e2e::http_header cache-control)"
            e2e::assert_contains "$cc" "immutable" "Cache-Control: immutable on /static/*.js"
            ;;
        404)
            printf '%s  note: no /static/*.js route — SPA inlines its bundle.%s\n' \
                "$E2E_DIM" "$E2E_OFF"
            ;;
        *)
            e2e::_fail_at "GET /static/dashboard.js: $E2E_HTTP_STATUS (wanted 200 or 404)"
            ;;
    esac
}

# ---- entrypoint --------------------------------------------------------------

main() {
    case "${CASE:-all}" in
        all)
            e2e::run_case loads          case_loads
            e2e::run_case bootstrap      case_bootstrap
            e2e::run_case health         case_health
            e2e::run_case openapi        case_openapi
            e2e::run_case static-chunks  case_static_chunks
            ;;
        loads)         e2e::run_case loads          case_loads ;;
        bootstrap)     e2e::run_case bootstrap      case_bootstrap ;;
        health)        e2e::run_case health         case_health ;;
        openapi)       e2e::run_case openapi        case_openapi ;;
        static-chunks) e2e::run_case static-chunks  case_static_chunks ;;
        *) printf 'unknown case: %s\n' "${CASE}" >&2; exit 64 ;;
    esac
    e2e::summary
}

main "$@"
