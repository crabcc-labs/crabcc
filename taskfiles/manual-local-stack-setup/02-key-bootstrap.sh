#!/usr/bin/env bash
# Stage 2 — generate + persist the LiteLLM master key.
. "$(dirname "$0")/lib.sh"

section "2. Key bootstrap"

out=$(install/ollama-stack/init-keys.sh 2>&1)
echo "$out" | grep -q "LITELLM_MASTER_KEY" && pass "init-keys.sh prints LITELLM_MASTER_KEY" \
    || fail "no LITELLM_MASTER_KEY block in stdout"

env_file="install/ollama-stack/.env"
[[ -f "$env_file" ]] && pass ".env exists" || fail ".env missing"
mode=$(stat -f '%Lp' "$env_file" 2>/dev/null || stat -c '%a' "$env_file" 2>/dev/null)
[[ "$mode" == "600" ]] && pass ".env mode 600" || fail ".env mode $mode (expected 600)"
grep -q '^OLLAMA_API_KEY=' "$env_file" && pass "OLLAMA_API_KEY present" || fail "OLLAMA_API_KEY missing"
grep -q '^LITELLM_MASTER_KEY=sk-' "$env_file" && pass "LITELLM_MASTER_KEY=sk- present" || fail "master key missing/malformed"

# Confirm gitignored.
if git check-ignore "$env_file" >/dev/null 2>&1; then
    pass ".env is gitignored"
else
    fail ".env NOT gitignored"
fi

# Auto-persisted user-facing key (v2.10.x).
ufkf="$HOME/.crabcc.local.api-key"
[[ -f "$ufkf" ]] && pass "$ufkf exists" || fail "$ufkf missing — init-keys.sh should auto-write"
ufmode=$(stat -f '%Lp' "$ufkf" 2>/dev/null || stat -c '%a' "$ufkf" 2>/dev/null)
[[ "$ufmode" == "400" ]] && pass "$ufkf mode 400" || fail "$ufkf mode $ufmode (expected 400)"

# --rotate flips the master key.
prev=$(grep '^LITELLM_MASTER_KEY=' "$env_file")
install/ollama-stack/init-keys.sh --rotate >/dev/null 2>&1
new=$(grep '^LITELLM_MASTER_KEY=' "$env_file")
[[ "$prev" != "$new" ]] && pass "--rotate changes master key" || fail "rotated key unchanged"

# --quiet emits only the key.
q=$(install/ollama-stack/init-keys.sh --quiet 2>&1 | wc -l | tr -d ' ')
[[ "$q" == "1" ]] && pass "--quiet emits one line" || fail "--quiet emitted $q lines"

report
