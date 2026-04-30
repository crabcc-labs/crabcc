#!/usr/bin/env bash
# Stage 4 — Caddy bearer-token gate (must reject unauth, accept auth).
. "$(dirname "$0")/lib.sh"
load_env

section "4. Auth gate"

base="${OLLAMA_BASE:-http://localhost:11435}"

must_http "/api/tags rejects unauth"  401 "$base/api/tags"
must_http "/v1/models rejects unauth" 401 "$base/v1/models"

if [[ -n "${OLLAMA_API_KEY:-}" ]]; then
    body=$(curl -fsS --max-time 10 -H "Authorization: Bearer $OLLAMA_API_KEY" "$base/api/tags" || echo '')
    if echo "$body" | jq -e '.models | type == "array"' >/dev/null 2>&1; then
        pass "/api/tags returns models[] with bearer"
    else
        fail "/api/tags didn't return models[] with bearer"
    fi
else
    fail "OLLAMA_API_KEY not loaded — run init-keys.sh first"
fi

# Internal /healthz is auth-free (only reachable from inside the
# compose network — exec into caddy to test).
hz=$(docker compose -f install/ollama-stack/docker-compose.yml exec -T caddy \
        wget -qO- http://localhost:11434/healthz 2>/dev/null || echo '')
[[ "$hz" == "ok" ]] && pass "internal /healthz returns ok" \
    || fail "internal /healthz didn't return ok (got: $hz)"

report
