#!/usr/bin/env bash
# Stage 6 — `crabcc ollama-stack` integration.
. "$(dirname "$0")/lib.sh"

section "6. crabcc ollama-stack integration"

if crabcc ollama-stack status --json 2>/dev/null | jq -e '. | type == "array"' >/dev/null; then
    pass "status --json returns array"
else
    fail "status --json didn't return JSON array"
fi

if crabcc ollama-stack logs caddy --tail 20 >/dev/null 2>&1; then
    pass "logs caddy --tail 20"
else
    fail "logs subcommand failed"
fi

if crabcc ollama-stack pull 2>/dev/null | jq -e '.ok == true' >/dev/null; then
    pass "pull returns {ok:true}"
else
    warn "pull didn't return {ok:true} (network? cold?)"
fi

# Down → up cycle.
crabcc ollama-stack down >/dev/null 2>&1 \
    && pass "down ok" || fail "down failed"
out=$(crabcc ollama-stack up 2>/dev/null)
if echo "$out" | jq -e '.services_healthy | length >= 3' >/dev/null 2>&1; then
    pass "up returns services_healthy[≥3]"
else
    fail "up didn't return services_healthy[]"
fi

report
