#!/usr/bin/env bash
# scripts/tests/e2e/knowledge.sh — memory drawer + ingest endpoints.
#
# Cases:
#   graph-empty-or-populated  GET  /api/memory/graph?limit=10
#   ingest-text               POST /api/memory/ingest text:* round-trips via /get
#   ingest-url                POST /api/memory/ingest external URL — net-flaky tolerant
#   ssrf-rejected             POST /api/memory/ingest 127.0.0.1 → errors[], no ingest
#
# Run all:        bash scripts/tests/e2e/knowledge.sh
# Run one:        CASE=ingest-text bash scripts/tests/e2e/knowledge.sh

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_SUITE_NAME="knowledge"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/_runner.e2e.sh"

e2e::preflight
e2e::ensure_server

# ---- cases -------------------------------------------------------------------

case_graph_empty_or_populated() {
    e2e::http GET '/api/memory/graph?limit=10'
    e2e::assert_status 200 "GET /api/memory/graph?limit=10"
    e2e::assert_json '.nodes | type' 'array' '.nodes is array'
    e2e::assert_json '.edges | type' 'array' '.edges is array'
    e2e::assert_json '.stats | type' 'object' '.stats is object'
    e2e::assert_json '.stats.embeddings' 'false' '.stats.embeddings == false'
    e2e::assert_json '.stats.drawers | type' 'number' '.stats.drawers numeric'
}

case_ingest_text() {
    local body='{"text":"hello e2e world","source":"e2e"}'
    e2e::http POST /api/memory/ingest "$body"
    e2e::assert_status 200 "POST /api/memory/ingest (text)"
    e2e::assert_json '(.ingested | length) > 0 | tostring' 'true' 'at least one drawer'
    e2e::assert_json '.errors | length' '0' 'no errors'

    # First text:* drawer round-trips via /api/memory/get.
    local id
    id="$(jq -r '.ingested[] | select(.id|startswith("text:")) | .id' "$E2E_HTTP_BODY" | head -n1)"
    e2e::assert_ne "$id" "" 'a text:* drawer was ingested'

    e2e::http GET "/api/memory/get?id=$id"
    e2e::assert_status 200 "GET /api/memory/get?id=$id"
    e2e::assert_json '.found'        'true'             'drawer found'
    e2e::assert_json '.id'           "$id"              'returned id matches'
    e2e::assert_json '.body'         'hello e2e world'  'body round-trips'
}

case_ingest_url() {
    # External fetches can be flaky from a dev box. We tolerate either
    # outcome as long as the response shape is correct: at least one
    # web:* drawer ingested OR the URL surfaces in errors[].
    local body='{"urls":["https://example.com"],"source":"e2e"}'
    e2e::http POST /api/memory/ingest "$body"
    e2e::assert_status 200 "POST /api/memory/ingest (urls)"

    local web_count err_count
    web_count="$(jq -r '[ .ingested[] | select(.id|startswith("web:")) ] | length' "$E2E_HTTP_BODY")"
    err_count="$(jq -r '.errors | length' "$E2E_HTTP_BODY")"

    if (( web_count == 0 && err_count == 0 )); then
        e2e::_fail_at "ingest-url: neither ingested[] nor errors[] mention example.com"
        printf '%s  --- response body ---%s\n' "$E2E_DIM" "$E2E_OFF" >&2
        head -c 800 "$E2E_HTTP_BODY" >&2; printf '\n' >&2
    fi
}

case_ssrf_rejected() {
    local body='{"urls":["http://127.0.0.1/admin"]}'
    e2e::http POST /api/memory/ingest "$body"
    e2e::assert_status 200 "POST /api/memory/ingest (ssrf)"
    e2e::assert_json '.ingested | length' '0' 'ingested[] is empty for SSRF'
    e2e::assert_json '(.errors | length) >= 1 | tostring' 'true' 'errors[] non-empty'
    e2e::assert_json '[.errors[].url] | index("http://127.0.0.1/admin") | tostring' \
        '0' 'errors[].url contains the rejected URL'
}

# ---- entrypoint --------------------------------------------------------------

main() {
    case "${CASE:-all}" in
        all)
            e2e::run_case graph-empty-or-populated case_graph_empty_or_populated
            e2e::run_case ingest-text              case_ingest_text
            e2e::run_case ingest-url               case_ingest_url
            e2e::run_case ssrf-rejected            case_ssrf_rejected
            ;;
        graph-empty-or-populated) e2e::run_case graph-empty-or-populated case_graph_empty_or_populated ;;
        ingest-text)              e2e::run_case ingest-text              case_ingest_text ;;
        ingest-url)               e2e::run_case ingest-url               case_ingest_url ;;
        ssrf-rejected)            e2e::run_case ssrf-rejected            case_ssrf_rejected ;;
        *) printf 'unknown case: %s\n' "${CASE}" >&2; exit 64 ;;
    esac
    e2e::summary
}

main "$@"
