#!/usr/bin/env bash
# Stage 5 — LiteLLM OpenAI-compat surface.
. "$(dirname "$0")/lib.sh"
load_env

section "5. LiteLLM front"

# Pull the model first (idempotent; 0 b on warm cache).
docker compose -f install/ollama-stack/docker-compose.yml exec -T ollama \
    ollama pull qwen2.5-coder >/dev/null 2>&1 \
    && pass "ollama pull qwen2.5-coder" \
    || warn "ollama pull failed (network? cold start?)"

base="${LITELLM_BASE:-http://localhost:4000}"
key="${LITELLM_MASTER_KEY:-}"
[[ -z "$key" ]] && { fail "LITELLM_MASTER_KEY not loaded"; report; exit 1; }

models=$(curl -fsS --max-time 10 -H "Authorization: Bearer $key" "$base/v1/models" 2>/dev/null \
        | jq -r '.data[]?.id' 2>/dev/null | wc -l | tr -d ' ')
[[ "$models" -ge 1 ]] && pass "$models models listed via /v1/models" \
    || fail "/v1/models didn't return >=1 model"

# Live chat completion — 1 token, lowest cost.
content=$(curl -fsS --max-time 60 \
    -H "Authorization: Bearer $key" \
    -H 'Content-Type: application/json' \
    -d '{"model":"ollama/qwen2.5-coder","max_tokens":4,"messages":[{"role":"user","content":"reply with the single word PONG"}]}' \
    "$base/v1/chat/completions" 2>/dev/null \
    | jq -r '.choices[0].message.content' 2>/dev/null)
if [[ -n "$content" ]]; then
    pass "/v1/chat/completions returned content: \"${content:0:32}…\""
else
    fail "/v1/chat/completions returned no content"
fi

report
