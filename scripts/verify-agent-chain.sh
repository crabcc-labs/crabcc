#!/usr/bin/env bash
# verify-agent-chain.sh — end-to-end smoke test for the
# agent → LiteLLM → Caddy → Ollama chain.
#
# Hits each hop in isolation, then a real `crabcc agent --dry-run`,
# then a 1-token live chat completion. Each step prints a single
# pass/fail line; final exit code is non-zero if anything failed.
#
# Usage:
#   bash scripts/verify-agent-chain.sh                  # human output
#   bash scripts/verify-agent-chain.sh --json           # machine
#   bash scripts/verify-agent-chain.sh --skip-live      # skip the
#                                                       # token-burning
#                                                       # generation step
#
# Preconditions:
#   - install/ollama-stack/.env exists (run init-keys.sh first)
#   - the stack is up: `crabcc ollama-stack up` or
#     `cd install/ollama-stack && docker compose up -d --wait`

set -uo pipefail

JSON=0
SKIP_LIVE=0
for arg in "$@"; do
    case "$arg" in
        --json|-j)        JSON=1 ;;
        --skip-live|-s)   SKIP_LIVE=1 ;;
        --help|-h)        sed -n '1,22p' "${BASH_SOURCE[0]:-$0}"; exit 0 ;;
    esac
done

if [[ -t 1 ]] && [[ $JSON -eq 0 ]]; then
    GRN='\033[32m'; RED='\033[31m'; DIM='\033[2m'; OFF='\033[0m'
else
    GRN=''; RED=''; DIM=''; OFF=''
fi

pass_count=0
fail_count=0
results=()

step() {
    local name="$1"; local expected="$2"; shift 2
    local out rc
    out="$("$@" 2>&1)"; rc=$?
    if [[ $rc -eq 0 ]]; then
        pass_count=$((pass_count + 1))
        if [[ $JSON -eq 0 ]]; then
            printf "  ${GRN}✓${OFF} %-40s ${DIM}%s${OFF}\n" "$name" "$expected"
        fi
        results+=("{\"step\":\"$name\",\"ok\":true}")
    else
        fail_count=$((fail_count + 1))
        if [[ $JSON -eq 0 ]]; then
            printf "  ${RED}✗${OFF} %-40s ${DIM}%s${OFF}\n" "$name" "$expected"
            printf "      %s\n" "$out" | head -5
        fi
        # JSON-escape the failure body
        local body; body="$(printf '%s' "$out" | head -3 | tr '\n' ' ' | sed 's/"/\\"/g')"
        results+=("{\"step\":\"$name\",\"ok\":false,\"detail\":\"$body\"}")
    fi
}

# --- 0. preconditions -----------------------------------------------------

if [[ -f install/ollama-stack/.env ]]; then
    # shellcheck disable=SC1091
    set -a; . install/ollama-stack/.env; set +a
elif [[ -f "$HOME/.crabcc/ollama-stack/.env" ]]; then
    set -a; . "$HOME/.crabcc/ollama-stack/.env"; set +a
fi
: "${OLLAMA_API_KEY:=}"
: "${LITELLM_MASTER_KEY:=$OLLAMA_API_KEY}"
: "${LITELLM_BASE:=http://localhost:4000}"
: "${OLLAMA_BASE:=http://localhost:11435}"   # Caddy port; not :11434

[[ $JSON -eq 0 ]] && printf "${DIM}LITELLM_BASE=%s  OLLAMA_BASE=%s${OFF}\n\n" "$LITELLM_BASE" "$OLLAMA_BASE"

# --- 1. containers up -----------------------------------------------------

containers_up() {
    local runtime
    if command -v container >/dev/null 2>&1; then
        runtime="container"
    elif command -v docker >/dev/null 2>&1; then
        runtime="docker"
    else
        echo "no container runtime on PATH"; return 1
    fi
    # Expect at least 3 running crabcc-labeled containers (ollama, caddy, litellm).
    local n
    n=$($runtime ls --format json 2>/dev/null \
        | jq -r '[ .[]? | select((.configuration.labels // .Labels // {} | tostring) | contains("com.crabcc")) | select((.status // .State) == "running") ] | length' \
        2>/dev/null || echo 0)
    if [[ "$n" -ge 3 ]]; then
        return 0
    fi
    echo "expected >=3 running crabcc-labeled containers, got $n"
    return 1
}
step "containers up" "ollama / caddy / litellm running with com.crabcc labels" containers_up

# --- 2. caddy /healthz (auth-free) ----------------------------------------

caddy_health() {
    # /healthz is unauthenticated; prove caddy is reachable + responding.
    local body
    body=$(curl -fsS --max-time 5 "$OLLAMA_BASE/healthz" 2>&1) || return 1
    [[ "$body" == "ok" ]]
}
step "caddy /healthz" "200 ok (auth-free)" caddy_health

# --- 3. caddy auth gate is enforced ---------------------------------------

caddy_auth_gate_rejects() {
    local code
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 \
        "$OLLAMA_BASE/api/tags")
    [[ "$code" == "401" ]]
}
step "caddy 401 without bearer" "auth gate enforced on /api/*" caddy_auth_gate_rejects

# --- 4. caddy → ollama with bearer ----------------------------------------

caddy_to_ollama() {
    [[ -z "$OLLAMA_API_KEY" ]] && { echo "OLLAMA_API_KEY unset"; return 1; }
    local body
    body=$(curl -fsS --max-time 10 -H "Authorization: Bearer $OLLAMA_API_KEY" \
        "$OLLAMA_BASE/api/tags") || return 1
    echo "$body" | jq -e '.models | type == "array"' >/dev/null 2>&1
}
step "caddy → ollama /api/tags" "auth'd request returns JSON {models:[…]}" caddy_to_ollama

# --- 5. litellm /v1/models ------------------------------------------------

litellm_models() {
    [[ -z "$LITELLM_MASTER_KEY" ]] && { echo "LITELLM_MASTER_KEY unset"; return 1; }
    local body
    body=$(curl -fsS --max-time 10 -H "Authorization: Bearer $LITELLM_MASTER_KEY" \
        "$LITELLM_BASE/v1/models") || return 1
    echo "$body" | jq -e '.data | length > 0' >/dev/null 2>&1
}
step "litellm /v1/models" "OpenAI-compat surface lists ≥1 model" litellm_models

# --- 6. agent dry-run --------------------------------------------------

agent_dry_run() {
    local out
    out=$(crabcc agent --run "noop" --dry-run --no-refresh --no-repomix 2>&1) || return 1
    echo "$out" | grep -q "ollama/qwen2.5-coder"
}
step "crabcc agent --dry-run" "default backend ollama, model qwen2.5-coder" agent_dry_run

# --- 7. live chat completion (1-token, opt-out) --------------------------

if [[ $SKIP_LIVE -eq 0 ]]; then
    live_chat() {
        local body
        body=$(curl -fsS --max-time 60 \
            -H "Authorization: Bearer $LITELLM_MASTER_KEY" \
            -H 'Content-Type: application/json' \
            -d '{"model":"ollama/qwen2.5-coder","max_tokens":4,"messages":[{"role":"user","content":"say PING"}]}' \
            "$LITELLM_BASE/v1/chat/completions") || return 1
        echo "$body" | jq -e '.choices[0].message.content' >/dev/null 2>&1
    }
    step "live chat completion" "/v1/chat/completions returns content" live_chat
fi

# --- 8. agent run row in _internal.db (best-effort) ----------------------

internal_db_has_recent_row() {
    local db="${HOME}/.crabcc/_internal.db"
    [[ -f "$db" ]] || { echo "no _internal.db at $db"; return 1; }
    # Look for any row created in the last 60 s. The dry-run above
    # writes a row via ManagerGuard.
    local count
    count=$(sqlite3 -readonly "$db" \
        "SELECT count(*) FROM cli_calls WHERE started_ts > strftime('%s','now') - 60;" 2>/dev/null || echo 0)
    [[ "$count" -ge 1 ]]
}
step "_internal.db cli_calls row" "ManagerGuard recorded the dry-run" internal_db_has_recent_row

# --- summary --------------------------------------------------------------

if [[ $JSON -eq 1 ]]; then
    arr_inner=$(IFS=,; echo "${results[*]}")
    printf '{"pass":%d,"fail":%d,"steps":[%s]}\n' "$pass_count" "$fail_count" "$arr_inner"
else
    printf "\n  %d passed, %d failed\n" "$pass_count" "$fail_count"
fi

[[ $fail_count -eq 0 ]] || exit 1
exit 0
