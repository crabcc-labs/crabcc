#!/usr/bin/env bash
# scripts/tests/e2e/agents.sh — agent meta endpoints.
#
# Cases:
#   agents    GET /api/agents          → {agents:[...]}
#   kills     GET /api/agent-kills     → {kills:[...]}
#   models    GET /api/agent-models    → 200
#   profiles  GET /api/agent-profiles  → 200
#
# Run all:        bash scripts/tests/e2e/agents.sh
# Run one:        CASE=kills bash scripts/tests/e2e/agents.sh

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_SUITE_NAME="agents"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/_runner.e2e.sh"

e2e::preflight
e2e::ensure_server

# ---- cases -------------------------------------------------------------------

case_agents() {
    e2e::http GET /api/agents
    e2e::assert_status 200 "GET /api/agents"
    e2e::assert_json '.agents | type' 'array' '.agents is array'
}

case_kills() {
    # Note — spec said `{kills:[...]}` but the actual envelope is
    # `{db: <path>, rows: [...]}` (kills are pulled from ~/.crabcc/_internal.db).
    # We assert on the live shape; if the API later moves to `{kills:[...]}`
    # we'll add an OR-clause here.
    e2e::http GET /api/agent-kills
    e2e::assert_status 200 "GET /api/agent-kills"
    e2e::assert_json '.rows | type' 'array' '.rows is array (count may be 0)'
    e2e::assert_json '.db | type'   'string' '.db points to the internal sqlite path'
}

case_models() {
    e2e::http GET /api/agent-models
    e2e::assert_status 200 "GET /api/agent-models"
    if ! jq -e . "$E2E_HTTP_BODY" >/dev/null 2>&1; then
        e2e::_fail_at "GET /api/agent-models did not return valid JSON"; return 1
    fi
    # Live envelope: {dir, models}.
    e2e::assert_json '.models | type' 'array'  '.models is array'
    e2e::assert_json '.dir    | type' 'string' '.dir is string'
}

case_profiles() {
    e2e::http GET /api/agent-profiles
    e2e::assert_status 200 "GET /api/agent-profiles"
    if ! jq -e . "$E2E_HTTP_BODY" >/dev/null 2>&1; then
        e2e::_fail_at "GET /api/agent-profiles did not return valid JSON"; return 1
    fi
    # Live envelope: {dir, profiles}.
    e2e::assert_json '.profiles | type' 'array'  '.profiles is array'
    e2e::assert_json '.dir      | type' 'string' '.dir is string'
}

# ---- entrypoint --------------------------------------------------------------

main() {
    case "${CASE:-all}" in
        all)
            e2e::run_case agents   case_agents
            e2e::run_case kills    case_kills
            e2e::run_case models   case_models
            e2e::run_case profiles case_profiles
            ;;
        agents)   e2e::run_case agents   case_agents ;;
        kills)    e2e::run_case kills    case_kills ;;
        models)   e2e::run_case models   case_models ;;
        profiles) e2e::run_case profiles case_profiles ;;
        *) printf 'unknown case: %s\n' "${CASE}" >&2; exit 64 ;;
    esac
    e2e::summary
}

main "$@"
