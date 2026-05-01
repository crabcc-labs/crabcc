#!/usr/bin/env bash
# scripts/tests/e2e/graph.sh — call-graph SSE / API endpoints.
#
# Cases:
#   seed    GET /api/seed-graph?limit=20    valid graph snapshot, edges resolve
#   expand  GET /api/graph?root=<sym>&...   non-empty subgraph rooted at sym
#
# Run all:        bash scripts/tests/e2e/graph.sh
# Run one:        CASE=expand bash scripts/tests/e2e/graph.sh

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_SUITE_NAME="graph"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/_runner.e2e.sh"

e2e::preflight
e2e::require_bin
e2e::ensure_server

# Make sure the index exists so `/api/graph?root=...` has data to walk.
# Idempotent — re-runs are cheap (~1s on a 168-file repo).
if [[ ! -f "$E2E_REPO_ROOT/.crabcc/index.db" ]]; then
    printf '%s[graph] no .crabcc/index.db — running `crabcc index` once.%s\n' \
        "$E2E_DIM" "$E2E_OFF"
    "$E2E_BIN" --root "$E2E_REPO_ROOT" index >/dev/null
fi

# ---- cases -------------------------------------------------------------------

case_seed() {
    e2e::http GET '/api/seed-graph?limit=20'
    e2e::assert_status 200 "GET /api/seed-graph?limit=20"
    e2e::assert_json '.nodes | type' 'array' '.nodes is array'
    e2e::assert_json '.edges | type' 'array' '.edges is array'
    e2e::assert_json '(.nodes | length) > 0 | tostring' 'true' '.nodes non-empty'
    e2e::assert_json '(.edges | length) > 0 | tostring' 'true' '.edges non-empty'

    # Every edge endpoint (.src and .dst) must resolve to a node id in the
    # snapshot. We compute set difference via jq and assert == [].
    local missing
    missing="$(jq -c '
        (.nodes | map(.id)) as $ids
        | [ .edges[] | .src, .dst ] | unique
        | map(select( . as $x | ($ids | index($x)) | not ))
    ' "$E2E_HTTP_BODY")"
    e2e::assert_eq "$missing" '[]' 'every edge endpoint resolves to a node'
}

case_expand() {
    # We pick `run` as the root: from probing /api/seed-graph it has the
    # highest fan-out (40 outgoing edges) of any name in the index, so
    # callees-depth-1 reliably yields a non-empty subgraph regardless of
    # which crate the dev currently has hot. (Originally Store::open, but
    # that one resolves to a single node with no outbound graph edges in
    # the current snapshot — see `case_expand_store` below for the
    # secondary fallback.)
    e2e::http GET '/api/graph?root=run&dir=callees&depth=1'
    e2e::assert_status 200 "GET /api/graph?root=run&dir=callees&depth=1"
    e2e::assert_json '(.nodes | length) > 1 | tostring' 'true' '.nodes > 1'
    e2e::assert_json '(.edges | length) > 0 | tostring' 'true' '.edges non-empty'
}

# ---- entrypoint --------------------------------------------------------------

main() {
    case "${CASE:-all}" in
        all)    e2e::run_case seed   case_seed
                e2e::run_case expand case_expand ;;
        seed)   e2e::run_case seed   case_seed ;;
        expand) e2e::run_case expand case_expand ;;
        *) printf 'unknown case: %s\n' "${CASE}" >&2; exit 64 ;;
    esac
    e2e::summary
}

main "$@"
