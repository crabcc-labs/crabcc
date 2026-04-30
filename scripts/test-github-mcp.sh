#!/usr/bin/env bash
# scripts/test-github-mcp.sh
#
# End-to-end test for the GitHub MCP server, BOTH local (Docker stdio) and
# remote (api.githubcopilot.com Streamable HTTP). Speaks raw MCP JSON-RPC 2.0
# so it does not depend on Claude Code restart timing.
#
# Test sequence per server:
#   1. initialize  → expect server capabilities
#   2. notifications/initialized
#   3. tools/call get_me  → expect a result with the authenticated user
#
# Exits 0 only if BOTH calls succeed. Run after scripts/setup-github-mcp.sh.

set -euo pipefail

readonly LOCAL_DOCKER_IMAGE="ghcr.io/github/github-mcp-server:latest"
readonly REMOTE_URL="https://api.githubcopilot.com/mcp/"
readonly TIMEOUT_SECS=30

c_blue='\033[1;34m'; c_yel='\033[1;33m'; c_red='\033[1;31m'; c_grn='\033[1;32m'; c_off='\033[0m'
log()  { printf "${c_blue}[test]${c_off} %s\n" "$*" >&2; }
ok()   { printf "${c_grn}[pass]${c_off} %s\n" "$*" >&2; }
fail() { printf "${c_red}[FAIL]${c_off} %s\n" "$*" >&2; }

# ── prereqs ────────────────────────────────────────────────────────────────
for c in gh jq curl docker; do
  command -v "$c" >/dev/null 2>&1 || { fail "missing: $c"; exit 1; }
done

GH_TOKEN="$(gh auth token 2>/dev/null || true)"
[[ -n "${GH_TOKEN:-}" ]] || { fail "no gh token — run: gh auth login"; exit 1; }

# ── JSON-RPC payloads ──────────────────────────────────────────────────────
# Compact, single-line so each is exactly one frame in stdio mode.
INIT_REQ=$(jq -nc \
  '{jsonrpc:"2.0", id:1, method:"initialize",
    params:{
      protocolVersion:"2024-11-05",
      capabilities:{},
      clientInfo:{name:"crabcc-mcp-test", version:"1.0"}
    }}')
INIT_NOTE=$(jq -nc '{jsonrpc:"2.0", method:"notifications/initialized"}')
GET_ME_REQ=$(jq -nc \
  '{jsonrpc:"2.0", id:2, method:"tools/call",
    params:{name:"get_me", arguments:{}}}')

# Decode SSE-or-JSON response bodies. The Streamable HTTP transport sometimes
# returns plain JSON, sometimes one or more `data: …` SSE events.
decode_body() {
  local body="$1"
  local data
  data="$(printf '%s\n' "$body" | sed -n 's/^data: //p' | head -1 || true)"
  [[ -n "$data" ]] && { printf '%s' "$data"; return; }
  printf '%s' "$body"
}

# ── REMOTE test ────────────────────────────────────────────────────────────
test_remote() {
  log "[remote] target = ${REMOTE_URL}"
  local hdrs body sid init_payload me_payload login

  hdrs="$(mktemp)"; trap 'rm -f "$hdrs"' RETURN

  # 1) initialize
  body="$(curl --max-time "$TIMEOUT_SECS" -sS -X POST "$REMOTE_URL" \
    -H "Authorization: Bearer ${GH_TOKEN}" \
    -H "Accept: application/json, text/event-stream" \
    -H "Content-Type: application/json" \
    -D "$hdrs" \
    --data "${INIT_REQ}")" || { fail "[remote] curl failed on initialize"; return 1; }

  init_payload="$(decode_body "$body")"
  if printf '%s' "$init_payload" | jq -e '.error' >/dev/null 2>&1; then
    fail "[remote] initialize returned error: $(printf '%s' "$init_payload" | jq -c '.error')"
    return 1
  fi
  printf '%s' "$init_payload" | jq -e '.result.serverInfo' >/dev/null 2>&1 \
    || { fail "[remote] initialize had no .result.serverInfo (body=${body:0:200})"; return 1; }

  sid="$(awk -F': ' 'tolower($1)=="mcp-session-id" {print $2}' "$hdrs" | tr -d '\r\n' || true)"
  ok "[remote] initialize OK (server=$(printf '%s' "$init_payload" | jq -rc '.result.serverInfo.name')${sid:+, session=${sid:0:8}…})"

  # 2) initialized notification (response body ignored)
  curl --max-time "$TIMEOUT_SECS" -sS -X POST "$REMOTE_URL" \
    -H "Authorization: Bearer ${GH_TOKEN}" \
    -H "Accept: application/json, text/event-stream" \
    -H "Content-Type: application/json" \
    ${sid:+-H "Mcp-Session-Id: ${sid}"} \
    --data "${INIT_NOTE}" >/dev/null

  # 3) tools/call get_me
  body="$(curl --max-time "$TIMEOUT_SECS" -sS -X POST "$REMOTE_URL" \
    -H "Authorization: Bearer ${GH_TOKEN}" \
    -H "Accept: application/json, text/event-stream" \
    -H "Content-Type: application/json" \
    ${sid:+-H "Mcp-Session-Id: ${sid}"} \
    --data "${GET_ME_REQ}")" || { fail "[remote] curl failed on tools/call"; return 1; }

  me_payload="$(decode_body "$body")"
  if printf '%s' "$me_payload" | jq -e '.error' >/dev/null 2>&1; then
    fail "[remote] get_me returned error: $(printf '%s' "$me_payload" | jq -c '.error')"
    return 1
  fi
  login="$(printf '%s' "$me_payload" | jq -r '.result.content[0].text' 2>/dev/null \
            | jq -r '.login // empty' 2>/dev/null || true)"
  if [[ -n "$login" ]]; then
    ok "[remote] get_me → login=${login}"
  else
    ok "[remote] get_me returned a result (login not in expected shape — server may have changed schema)"
  fi
}

# ── LOCAL test (docker stdio) ──────────────────────────────────────────────
test_local() {
  log "[local] image = ${LOCAL_DOCKER_IMAGE}"
  local out me_payload login

  out="$(printf '%s\n%s\n%s\n' "${INIT_REQ}" "${INIT_NOTE}" "${GET_ME_REQ}" \
    | timeout "$TIMEOUT_SECS" docker run -i --rm \
        -e GITHUB_PERSONAL_ACCESS_TOKEN="${GH_TOKEN}" \
        "$LOCAL_DOCKER_IMAGE" 2>/dev/null \
    || true)"

  if [[ -z "$out" ]]; then
    fail "[local] container produced no stdout (is the image healthy? try: docker run --rm ${LOCAL_DOCKER_IMAGE} --help)"
    return 1
  fi

  # Each line is one JSON-RPC message. Find id=2 (the get_me response).
  me_payload="$(printf '%s\n' "$out" | jq -cs 'map(select(.id==2)) | .[0] // empty' 2>/dev/null || true)"
  if [[ -z "$me_payload" || "$me_payload" == "null" ]]; then
    fail "[local] no id=2 response in stdout. raw (first 400 bytes): ${out:0:400}"
    return 1
  fi

  if printf '%s' "$me_payload" | jq -e '.error' >/dev/null 2>&1; then
    fail "[local] get_me returned error: $(printf '%s' "$me_payload" | jq -c '.error')"
    return 1
  fi
  login="$(printf '%s' "$me_payload" | jq -r '.result.content[0].text' 2>/dev/null \
            | jq -r '.login // empty' 2>/dev/null || true)"
  if [[ -n "$login" ]]; then
    ok "[local] get_me → login=${login}"
  else
    ok "[local] get_me returned a result (login not in expected shape)"
  fi
}

# ── run both, accumulate failures ──────────────────────────────────────────
fails=0
test_remote || fails=$((fails+1))
echo
test_local  || fails=$((fails+1))
echo

if (( fails > 0 )); then
  fail "${fails} of 2 servers failed"
  exit 1
fi
ok "both servers passed JSON-RPC initialize + get_me"
