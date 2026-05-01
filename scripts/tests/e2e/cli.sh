#!/usr/bin/env bash
# scripts/tests/e2e/cli.sh — `crabcc` CLI smoke.
#
# Cases:
#   version              ./target/release/crabcc --version  → "crabcc <Cargo.toml-version>"
#   index-smoke          ./target/release/crabcc lookup sym Store  → non-empty result
#                        (auto-runs `crabcc index` if .crabcc/index.db is missing)
#   memory-ingest-text   crabcc memory ingest --text "..."   → text:* drawer ingested
#   memory-ingest-url    crabcc memory ingest --url URL      → web:* drawer ingested
#                        (or clean error envelope if the URL is unreachable)
#   memory-ingest-stdin  echo "..." | crabcc memory ingest --stdin --text " " → text drawer
#
# Run all:        bash scripts/tests/e2e/cli.sh
# Run one:        CASE=memory-ingest-text bash scripts/tests/e2e/cli.sh

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_SUITE_NAME="cli"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/_runner.e2e.sh"

e2e::preflight
e2e::require_bin

# Read [workspace.package].version via the canonical script.
_cli_workspace_version() {
    bash "$E2E_REPO_ROOT/scripts/version.sh"
}

# ---- cases -------------------------------------------------------------------

case_version() {
    local actual; actual="$("$E2E_BIN" --version 2>&1 | head -n1)"
    local expected; expected="$(_cli_workspace_version)"
    e2e::assert_ne "$expected" "" "Cargo.toml workspace version found"
    e2e::assert_eq "$actual" "crabcc $expected" "crabcc --version matches Cargo.toml"
}

case_index_smoke() {
    if [[ ! -f "$E2E_REPO_ROOT/.crabcc/index.db" ]]; then
        printf '%s[cli] no .crabcc/index.db — running `crabcc index` once.%s\n' \
            "$E2E_DIM" "$E2E_OFF"
        "$E2E_BIN" --root "$E2E_REPO_ROOT" index >/dev/null
    fi

    # Note: `crabcc sym X` is the deprecated alias; the canonical command is
    # `crabcc lookup sym X`. Both return JSON; we use the canonical form so
    # we don't fail when the deprecation finally goes away. There is also no
    # `--json` flag on this subcommand — the default output IS json.
    local out; out="$("$E2E_BIN" --root "$E2E_REPO_ROOT" lookup sym Store 2>&1)"
    if ! printf '%s' "$out" | jq -e . >/dev/null 2>&1; then
        e2e::_fail_at "lookup sym Store: not valid JSON"
        printf '  output: %s\n' "$out" >&2
        return 1
    fi

    local len; len="$(printf '%s' "$out" | jq 'length')"
    if (( len < 1 )); then
        e2e::_fail_at "lookup sym Store: expected at least one result, got $len"
        return 1
    fi

    # Sanity: result entry has a name field equal to "Store".
    local name; name="$(printf '%s' "$out" | jq -r '.[0].name')"
    e2e::assert_eq "$name" "Store" "first result name == Store"
}

# `crabcc memory ingest` exposes the same wire shape as POST /api/memory/ingest:
# `{ ingested[], errors[], stats{ok,failed} }`. The CLI subcommand is
# additive — running it leaves drawers in `.crabcc/memory.db` exactly the
# way the dashboard's IngestBox does.
case_memory_ingest_text() {
    local out; out="$("$E2E_BIN" --root "$E2E_REPO_ROOT" memory ingest \
        --source "e2e-cli" --text "hello e2e $(date +%s)" 2>&1)"
    if ! printf '%s' "$out" | jq -e . >/dev/null 2>&1; then
        e2e::_fail_at "memory ingest --text: not valid JSON"
        printf '  output: %s\n' "$out" >&2
        return 1
    fi
    local ok; ok="$(printf '%s' "$out" | jq '.stats.ok')"
    local kind; kind="$(printf '%s' "$out" | jq -r '.ingested[0].kind // ""')"
    e2e::assert_eq "$ok" "1" "ingested 1 drawer"
    e2e::assert_eq "$kind" "text" "kind == text"
    local id; id="$(printf '%s' "$out" | jq -r '.ingested[0].id')"
    e2e::assert_contains "$id" "text:" "drawer id is text:<hash>"
}

case_memory_ingest_url() {
    local out; out="$("$E2E_BIN" --root "$E2E_REPO_ROOT" memory ingest \
        --source "e2e-cli" --url "https://example.com" 2>&1)"
    if ! printf '%s' "$out" | jq -e . >/dev/null 2>&1; then
        e2e::_fail_at "memory ingest --url: not valid JSON"
        printf '  output: %s\n' "$out" >&2
        return 1
    fi
    # Pass either: the URL was fetched and ingested, OR a clean error
    # envelope was returned. Network may be flaky in dev — tolerate both.
    local ok; ok="$(printf '%s' "$out" | jq '.stats.ok')"
    local failed; failed="$(printf '%s' "$out" | jq '.stats.failed')"
    if (( ok == 1 )); then
        local id; id="$(printf '%s' "$out" | jq -r '.ingested[0].id')"
        e2e::assert_contains "$id" "web:" "drawer id is web:<hash>"
    elif (( failed == 1 )); then
        local err; err="$(printf '%s' "$out" | jq -r '.errors[0].error')"
        e2e::assert_ne "$err" "" "error has a reason"
    else
        e2e::_fail_at "memory ingest --url: ok=$ok failed=$failed (expected exactly one)"
        printf '  output: %s\n' "$out" >&2
        return 1
    fi
}

case_memory_ingest_stdin() {
    # `--stdin` reads from stdin AND can be combined with `--text` (both
    # are concatenated). We pass only stdin to verify the bare-stdin path.
    local payload; payload="hello e2e stdin $(date +%s)"
    local out
    out="$(printf '%s' "$payload" | "$E2E_BIN" --root "$E2E_REPO_ROOT" memory ingest \
        --source "e2e-cli" --stdin 2>&1)"
    if ! printf '%s' "$out" | jq -e . >/dev/null 2>&1; then
        e2e::_fail_at "memory ingest --stdin: not valid JSON"
        printf '  output: %s\n' "$out" >&2
        return 1
    fi
    local ok; ok="$(printf '%s' "$out" | jq '.stats.ok')"
    local kind; kind="$(printf '%s' "$out" | jq -r '.ingested[0].kind // ""')"
    e2e::assert_eq "$ok" "1" "stdin → 1 drawer"
    e2e::assert_eq "$kind" "text" "stdin kind == text"
}

# Composes the upstream `c7` (npm @vedanth/context7) docs CLI with our
# `crabcc memory ingest --stdin` path. This is the wired-up version of
# `task docs:ingest LIB=…`. We use `npx -y` so the test doesn't depend
# on a global install. Skipped (not failed) when `npx` is missing.
case_docs_ingest_pipe() {
    if ! command -v npx >/dev/null 2>&1; then
        printf '%s[cli] npx not on PATH — skipping docs-ingest-pipe.%s\n' \
            "$E2E_DIM" "$E2E_OFF"
        e2e::_log "docs-ingest-pipe: SKIP (npx missing)"
        return 0
    fi
    # `htmx` is small + stable + has an unambiguous c7 hit, so the
    # whole pipe round-trips in <10s in cache, ~30s cold.
    local out
    if ! out="$(npx -y @vedanth/context7 docs htmx --tokens 800 2>/dev/null \
        | "$E2E_BIN" --root "$E2E_REPO_ROOT" memory ingest \
            --stdin --source "e2e-c7" 2>&1)"
    then
        e2e::_fail_at "c7 → memory ingest pipe failed (network or c7 outage?)"
        printf '  output: %s\n' "$out" >&2
        return 1
    fi
    if ! printf '%s' "$out" | jq -e . >/dev/null 2>&1; then
        e2e::_fail_at "docs-ingest-pipe: not valid JSON"
        printf '  output: %s\n' "$out" >&2
        return 1
    fi
    local ok; ok="$(printf '%s' "$out" | jq '.stats.ok')"
    if (( ok < 1 )); then
        e2e::_fail_at "docs-ingest-pipe: ok=$ok (expected ≥1)"
        printf '  output: %s\n' "$out" >&2
        return 1
    fi
}

# ---- entrypoint --------------------------------------------------------------

main() {
    case "${CASE:-all}" in
        all)
            e2e::run_case version              case_version
            e2e::run_case index-smoke          case_index_smoke
            e2e::run_case memory-ingest-text   case_memory_ingest_text
            e2e::run_case memory-ingest-url    case_memory_ingest_url
            e2e::run_case memory-ingest-stdin  case_memory_ingest_stdin
            e2e::run_case docs-ingest-pipe     case_docs_ingest_pipe
            ;;
        version)              e2e::run_case version              case_version ;;
        index-smoke)          e2e::run_case index-smoke          case_index_smoke ;;
        memory-ingest-text)   e2e::run_case memory-ingest-text   case_memory_ingest_text ;;
        memory-ingest-url)    e2e::run_case memory-ingest-url    case_memory_ingest_url ;;
        memory-ingest-stdin)  e2e::run_case memory-ingest-stdin  case_memory_ingest_stdin ;;
        docs-ingest-pipe)     e2e::run_case docs-ingest-pipe     case_docs_ingest_pipe ;;
        *) printf 'unknown case: %s\n' "${CASE}" >&2; exit 64 ;;
    esac
    e2e::summary
}

main "$@"
