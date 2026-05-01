#!/usr/bin/env bash
# scripts/tests/e2e/fetch.sh — `crabcc fetch` CLI.
#
# Cases:
#   cli-fetch-example  ./target/release/crabcc fetch https://example.com
#                      → JSON with content_markdown containing "Example Domain"
#
# Run all:        bash scripts/tests/e2e/fetch.sh
# Run one:        CASE=cli-fetch-example bash scripts/tests/e2e/fetch.sh

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_SUITE_NAME="fetch"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/_runner.e2e.sh"

e2e::preflight
e2e::require_bin
# This suite is CLI-only — no need to ensure the dashboard server.

# ---- cases -------------------------------------------------------------------

case_cli_fetch_example() {
    local out_file="${TMPDIR:-/tmp}/crabcc-e2e-fetch.json"
    : > "$out_file"
    if ! "$E2E_BIN" --root "$E2E_REPO_ROOT" fetch --no-chrome \
            https://example.com > "$out_file" 2>/dev/null; then
        e2e::_fail_at "crabcc fetch exited non-zero"
        head -c 500 "$out_file" >&2; printf '\n' >&2
        return 1
    fi

    if ! jq -e . "$out_file" >/dev/null 2>&1; then
        e2e::_fail_at "crabcc fetch output is not valid JSON"
        head -c 500 "$out_file" >&2; printf '\n' >&2
        return 1
    fi

    local md
    md="$(jq -r '.[0].content_markdown // ""' "$out_file")"
    e2e::assert_contains "$md" "Example Domain" "content_markdown contains 'Example Domain'"
}

# ---- entrypoint --------------------------------------------------------------

main() {
    case "${CASE:-all}" in
        all|cli-fetch-example) e2e::run_case cli-fetch-example case_cli_fetch_example ;;
        *) printf 'unknown case: %s\n' "${CASE}" >&2; exit 64 ;;
    esac
    e2e::summary
}

main "$@"
