#!/usr/bin/env bash
# Stage 7 — ccc combo CLI parity with crabcc ollama-stack.
. "$(dirname "$0")/lib.sh"

section "7. ccc setup --ollama-* parity"

if ! command -v ccc >/dev/null 2>&1; then
    fail "ccc not on PATH (build crabcc-cli)"
    report
    exit 1
fi

a=$(ccc setup --ollama-status 2>/dev/null | jq -S '.' 2>/dev/null | head -10)
b=$(crabcc ollama-stack status 2>/dev/null | jq -S '.' 2>/dev/null | head -10)
[[ "$a" == "$b" ]] && pass "ccc --ollama-status == crabcc ollama-stack status" \
    || fail "outputs diverge"

ccc setup --help 2>&1 | grep -q -- '--ollama-up' && pass "--ollama-up listed" \
    || fail "--ollama-up missing from ccc setup --help"
ccc setup --help 2>&1 | grep -q -- '--ollama-down' && pass "--ollama-down listed" \
    || fail "--ollama-down missing"
ccc setup --help 2>&1 | grep -q -- '--ollama-pull' && pass "--ollama-pull listed" \
    || fail "--ollama-pull missing"

report
