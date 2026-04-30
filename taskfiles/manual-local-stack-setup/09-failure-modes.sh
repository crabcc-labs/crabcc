#!/usr/bin/env bash
# Stage 9 — negative coverage. Verifies sane failure modes.
. "$(dirname "$0")/lib.sh"

section "9. Failure modes"

# 9.1 — stack DOWN should auto-bring-up (idempotent).
crabcc ollama-stack down >/dev/null 2>&1
out=$(crabcc agent --run "ping" --backend ollama --no-refresh --no-repomix 2>&1 | head -3)
echo "$out" | grep -qE "(ensure_up|stack ready|services_healthy)" \
    && pass "agent triggers ensure_up when stack down" \
    || warn "auto-bring-up signal not detected: $out"

# 9.2 — tampered key returns 401.
load_env
if [[ -n "${OLLAMA_API_KEY:-}" ]]; then
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 \
        -H "Authorization: Bearer not-a-real-key" \
        "${OLLAMA_BASE:-http://localhost:11435}/api/tags")
    [[ "$code" == "401" ]] && pass "tampered bearer → 401" \
        || fail "tampered bearer returned $code (expected 401)"
fi

# 9.3 — Docker NOT running: emulated by hitting an unreachable port.
# (We don't actually stop docker — that would derail a longer chain.)
warn "9.3 'docker not running' is manual — stop the daemon yourself + re-run"

report
