#!/usr/bin/env bash
# Appendix B.2 — LiteLLM auth + alias passthrough (raw curl, no agent).
# Isolates the proxy from the agent runtime to confirm the same env
# vars the runtime forwards work end-to-end against LiteLLM.
. "$(dirname "$0")/lib.sh"
load_env

section "B.2 — LiteLLM auth + alias"

key="${OLLAMA_API_KEY:-${LITELLM_MASTER_KEY:-}}"
base="${OLLAMA_BASE_URL:-http://localhost:4000}"
[[ -z "$key" ]] && { fail "no key in env"; report; exit 1; }

# 1. Right key + valid model → expect content with PONG.
content=$(curl -fsS --max-time 30 \
    -H "Authorization: Bearer $key" \
    -H 'Content-Type: application/json' \
    -d '{"model":"qwen2.5-coder","max_tokens":4,"messages":[{"role":"user","content":"reply with the word PONG only"}]}' \
    "$base/v1/chat/completions" 2>/dev/null \
    | jq -r '.choices[0].message.content' 2>/dev/null)
echo "$content" | grep -qi 'pong' \
    && pass "right key + valid model → PONG-ish content" \
    || fail "expected PONG content, got: $content"

# 2. Wrong key → 401.
must_http "wrong key → 401" 401 "$base/v1/chat/completions" \
    -H "Authorization: Bearer NOT-A-REAL-KEY" \
    -H "Content-Type: application/json" \
    -d '{"model":"qwen2.5-coder","messages":[{"role":"user","content":"x"}]}'

# 3. Wrong model → 4xx (NOT 500/hang).
code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 \
    -H "Authorization: Bearer $key" \
    -H "Content-Type: application/json" \
    -d '{"model":"this-model-does-not-exist","messages":[{"role":"user","content":"x"}]}' \
    "$base/v1/chat/completions")
[[ "$code" =~ ^4 ]] && pass "wrong model → HTTP $code (4xx)" \
    || fail "wrong model → HTTP $code (expected 4xx)"

report
