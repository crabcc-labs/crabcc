#!/usr/bin/env bash
# tests/integration/test_mcp_bridge.sh — MCP bridge e2e integration test
#
# Starts supergateway wrapping `crabcc --mcp`, exercises the Streamable HTTP
# transport, then tears down. Validates tools/list and tools/call round-trips.
#
# Usage:
#   bash tests/integration/test_mcp_bridge.sh
#   PORT=9099 bash tests/integration/test_mcp_bridge.sh  # custom port
#
# Exit codes:
#   0  all assertions pass
#   1  any assertion failed
#   2  prerequisites missing
set -euo pipefail

PORT="${PORT:-9099}"
CRABCC="${CRABCC:-crabcc}"
PASS=0
FAIL=0
ERRORS=()

# ── helpers ───────────────────────────────────────────────────────────────────
log()  { printf "  %s\n" "$*"; }
ok()   { PASS=$((PASS+1)); printf "  \033[32m✓\033[0m %s\n" "$*"; }
fail() { FAIL=$((FAIL+1)); ERRORS+=("$*"); printf "  \033[31m✗\033[0m %s\n" "$*"; }

assert_json_field() {
  local label="$1" json="$2" field="$3" expected="$4"
  local actual
  actual=$(printf '%s' "$json" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d$field)" 2>/dev/null || echo "PARSE_ERROR")
  if [ "$actual" = "$expected" ]; then
    ok "$label"
  else
    fail "$label — expected '$expected', got '$actual'"
  fi
}

mcp_post() {
  curl -fsS -X POST "http://localhost:${PORT}/mcp" \
    -H "Content-Type: application/json" \
    -d "$1"
}

# ── preflight ─────────────────────────────────────────────────────────────────
echo "MCP bridge integration test  (port $PORT)"
echo ""

command -v npx      >/dev/null || { echo "npx missing — brew install node"; exit 2; }
command -v crabcc   >/dev/null || { echo "crabcc not on PATH — task install"; exit 2; }
command -v python3  >/dev/null || { echo "python3 missing"; exit 2; }

# ── setup: start supergateway ─────────────────────────────────────────────────
FIXTURE=$(mktemp -d)
cat > "$FIXTURE/a.ts" <<'EOF'
export function greet(name: string) { return `hello ${name}`; }
greet("world");
EOF
"$CRABCC" index --root "$FIXTURE" >/dev/null 2>&1 || true  # pre-warm index

npx -y @modelcontextprotocol/supergateway \
  --stdio "$CRABCC --root $FIXTURE --mcp" \
  --port "$PORT" \
  --outputTransport streamableHttp \
  --streamableHttpPath /mcp \
  --healthEndpoint /healthz \
  --cors \
  --logLevel none \
  2>/dev/null &
SGPID=$!
trap 'kill $SGPID 2>/dev/null; rm -rf "$FIXTURE"' EXIT

# Wait for bridge
for i in $(seq 1 15); do
  curl -sf "http://localhost:$PORT/healthz" >/dev/null && break
  sleep 0.5
done
curl -sf "http://localhost:$PORT/healthz" >/dev/null || { echo "Bridge did not start"; exit 1; }

echo "── health ────────────────────────────────────────────────────────────────"

HEALTH=$(curl -sf "http://localhost:$PORT/healthz")
[ "$HEALTH" = "ok" ] && ok "/healthz → ok" || fail "/healthz returned '$HEALTH'"

echo ""
echo "── tools/list ────────────────────────────────────────────────────────────"

LIST=$(mcp_post '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}')
TOOL_COUNT=$(printf '%s' "$LIST" | python3 -c "import sys,json; print(len(json.load(sys.stdin)['result']['tools']))" 2>/dev/null || echo 0)
[ "$TOOL_COUNT" -gt 0 ] && ok "tools/list returns $TOOL_COUNT tools" || fail "tools/list returned 0 tools"

# Verify expected tools are present
for TOOL in sym refs callers outline fuzzy; do
  HAS=$(printf '%s' "$LIST" | python3 -c "
import sys,json
d=json.load(sys.stdin)
names=[t['name'] for t in d['result']['tools']]
print('yes' if '$TOOL' in names else 'no')
" 2>/dev/null)
  [ "$HAS" = "yes" ] && ok "tool '$TOOL' listed" || fail "tool '$TOOL' missing from tools/list"
done

echo ""
echo "── tools/call: sym ───────────────────────────────────────────────────────"

SYM=$(mcp_post '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"sym","arguments":{"name":"greet"}}}')
SYM_TEXT=$(printf '%s' "$SYM" | python3 -c "
import sys,json
d=json.load(sys.stdin)
content=d.get('result',{}).get('content',[])
print(content[0]['text'] if content else '')
" 2>/dev/null)
if printf '%s' "$SYM_TEXT" | python3 -c "import sys,json; d=json.load(sys.stdin); exit(0 if any(s.get('name')=='greet' for s in d) else 1)" 2>/dev/null; then
  ok "sym greet → found definition"
else
  fail "sym greet → no result (got: ${SYM_TEXT:0:80})"
fi

echo ""
echo "── tools/call: callers ───────────────────────────────────────────────────"

CALLERS=$(mcp_post '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"callers","arguments":{"name":"greet","count":true}}}')
COUNT=$(printf '%s' "$CALLERS" | python3 -c "
import sys,json
d=json.load(sys.stdin)
content=d.get('result',{}).get('content',[])
j=json.loads(content[0]['text']) if content else {}
print(j.get('count',0))
" 2>/dev/null || echo 0)
[ "$COUNT" -ge 1 ] && ok "callers greet → count=$COUNT" || fail "callers greet → count=$COUNT (expected ≥1)"

echo ""
echo "── jsonrpc error handling ────────────────────────────────────────────────"

BAD=$(mcp_post '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nonexistent","arguments":{}}}')
HAS_ERR=$(printf '%s' "$BAD" | python3 -c "
import sys,json
d=json.load(sys.stdin)
print('yes' if 'error' in d or 'error' in str(d.get('result',{})) else 'no')
" 2>/dev/null)
[ "$HAS_ERR" = "yes" ] && ok "unknown tool returns error" || fail "unknown tool did not return error"

echo ""
echo "── summary ───────────────────────────────────────────────────────────────"
echo "  passed: $PASS   failed: $FAIL"
if [ ${#ERRORS[@]} -gt 0 ]; then
  echo ""
  echo "  Failures:"
  for e in "${ERRORS[@]}"; do echo "    ✗ $e"; done
fi
[ "$FAIL" -eq 0 ]
