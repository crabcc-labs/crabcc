#!/usr/bin/env bash
# scripts/ollama-network-check.sh — verify a network-exposed Ollama
# instance is reachable, healthy, and has the configured model present.
# For LAN / VPN / remote-host setups (e.g. Ollama running on a beefier
# Mac Studio while the dev box is a laptop).
#
# Distinct from ollama-system-check.sh:
#   system-check  → runs ON the Ollama host, measures RAM / disk / arch.
#   network-check → runs FROM a client, probes a remote daemon.
#
# Checks (in order, fail-fast):
#   1. URL parse + host resolution (DNS / mDNS / hosts file).
#   2. TCP reachability on the daemon port (default 11434).
#   3. /api/version returns 200 + a recognizable Ollama version string.
#   4. /api/tags lists at least one model.
#   5. Configured model is present in the tag list.
#   6. Optional /api/generate round-trip with a 1-token completion
#      (--smoke). Validates real inference works, not just the listing.
#
# Usage:
#   bash scripts/ollama-network-check.sh
#   bash scripts/ollama-network-check.sh --host http://ollama.lan:11434
#   bash scripts/ollama-network-check.sh --host … --model voytas26/…
#   bash scripts/ollama-network-check.sh --host … --smoke
#
# Env: OLLAMA_HOST, CRABCC_OLLAMA_MODEL.
#
# Exit codes:
#   0  OK         — every check passed
#   1  WARN       — daemon reachable, model missing (pull on remote first)
#   2  FAIL       — daemon unreachable / wrong service / TLS error
#   3  SMOKE-FAIL — daemon listed model but inference returned an error
set -euo pipefail

HOST="${OLLAMA_HOST:-http://127.0.0.1:11434}"
MODEL="${CRABCC_OLLAMA_MODEL:-voytas26/openclaw-oss-20b-deterministic}"
SMOKE=0
TIMEOUT=10

while [ $# -gt 0 ]; do
  case "$1" in
    --host)    HOST="$2"; shift 2 ;;
    --model)   MODEL="$2"; shift 2 ;;
    --timeout) TIMEOUT="$2"; shift 2 ;;
    --smoke)   SMOKE=1; shift ;;
    -h|--help) sed -n '1,30p' "$0" | sed 's/^# \?//'; exit 0 ;;
    *) echo "network-check: unknown arg $1" >&2; exit 2 ;;
  esac
done

for tool in curl jq; do
  command -v "$tool" >/dev/null || { echo "network-check: missing tool: $tool" >&2; exit 2; }
done

# ── render helpers ─────────────────────────────────────────────────────
if [ -t 1 ]; then COLOR=1; else COLOR=0; fi
green()  { [ "$COLOR" = "1" ] && printf '\033[32m✓\033[0m' || printf '✓'; }
yellow() { [ "$COLOR" = "1" ] && printf '\033[33m⚠\033[0m' || printf '⚠'; }
red()    { [ "$COLOR" = "1" ] && printf '\033[31m✗\033[0m' || printf '✗'; }

# ── 1. URL parse + host resolve ───────────────────────────────────────
echo "── ollama network check ──"
echo "host:          $HOST"
echo "model:         $MODEL"
echo ""

scheme="${HOST%%://*}"
hostport="${HOST#*://}"
hostport="${hostport%%/*}"
host_name="${hostport%%:*}"
port="${hostport##*:}"
[ "$port" = "$host_name" ] && port=$([ "$scheme" = "https" ] && echo 443 || echo 11434)

case "$scheme" in
  http|https) printf '%s  scheme:      %s\n' "$(green)" "$scheme" ;;
  *)          printf '%s  scheme:      %s (expected http/https)\n' "$(red)" "$scheme"; exit 2 ;;
esac

# DNS / hosts / mDNS resolution.
if getent_out=$(getent hosts "$host_name" 2>/dev/null) || \
   getent_out=$(dscacheutil -q host -a name "$host_name" 2>/dev/null | grep ip_address | head -1) || \
   getent_out=$(host "$host_name" 2>/dev/null); then
  printf '%s  resolve:     %s → %s\n' "$(green)" "$host_name" "$(echo "$getent_out" | head -1)"
else
  printf '%s  resolve:     %s did not resolve (DNS / hosts / .local mDNS all failed)\n' "$(red)" "$host_name"
  exit 2
fi

# ── 2. TCP reachability ───────────────────────────────────────────────
if (exec 3<>/dev/tcp/"$host_name"/"$port") 2>/dev/null; then
  printf '%s  tcp:         %s:%s open\n' "$(green)" "$host_name" "$port"
  exec 3>&- 3<&- 2>/dev/null || true
else
  printf '%s  tcp:         %s:%s NOT reachable (firewall? service down? wrong port?)\n' "$(red)" "$host_name" "$port"
  exit 2
fi

# ── 3. /api/version ───────────────────────────────────────────────────
ver_json=$(curl -fsS --max-time "$TIMEOUT" "$HOST/api/version" 2>&1) || {
  printf '%s  /api/version: HTTP failure: %s\n' "$(red)" "$ver_json"
  echo "" && echo "verdict: FAIL — daemon not reachable / wrong service / TLS error" >&2
  exit 2
}
ver=$(echo "$ver_json" | jq -r '.version // empty')
if [ -n "$ver" ]; then
  printf '%s  /api/version: ollama %s\n' "$(green)" "$ver"
else
  printf '%s  /api/version: 200 OK but no .version field — wrong service?\n' "$(red)"
  exit 2
fi

# ── 4. /api/tags ──────────────────────────────────────────────────────
tags_json=$(curl -fsS --max-time "$TIMEOUT" "$HOST/api/tags" 2>&1) || {
  printf '%s  /api/tags:   HTTP failure: %s\n' "$(red)" "$tags_json"
  exit 2
}
tag_count=$(echo "$tags_json" | jq -r '.models | length')
if [ "$tag_count" -gt 0 ]; then
  printf '%s  /api/tags:   %d model(s) installed on remote\n' "$(green)" "$tag_count"
else
  printf '%s  /api/tags:    0 models installed — `ollama pull %s` on the remote host\n' "$(yellow)" "$MODEL"
fi

# ── 5. configured model present? ──────────────────────────────────────
model_present=$(echo "$tags_json" | jq -r --arg m "$MODEL" '.models[]? | select(.name == $m) | .name' | head -1)
if [ -n "$model_present" ]; then
  size_gb=$(echo "$tags_json" | jq -r --arg m "$MODEL" '.models[] | select(.name == $m) | (.size/1e9 | floor)')
  printf '%s  model:       %s present (~%d GB)\n' "$(green)" "$MODEL" "$size_gb"
else
  printf '%s  model:       %s NOT installed on remote\n' "$(yellow)" "$MODEL"
  echo "                run on the Ollama host:  ollama pull $MODEL"
  if [ "$SMOKE" = "1" ]; then
    echo "                (skipping --smoke since model is missing)"
    SMOKE=0
  fi
  echo "" && echo "verdict: WARN — daemon healthy but model not present" && exit 1
fi

# ── 6. optional inference smoke ───────────────────────────────────────
if [ "$SMOKE" = "1" ]; then
  body=$(jq -nc --arg m "$MODEL" '{model:$m, prompt:"Say only the single word: OK", stream:false, options:{num_predict:8, temperature:0}}')
  reply=$(curl -fsS --max-time 60 -H 'Content-Type: application/json' -d "$body" "$HOST/api/generate" 2>&1) || {
    printf '%s  smoke:       /api/generate failed: %s\n' "$(red)" "$reply"
    exit 3
  }
  resp=$(echo "$reply" | jq -r '.response // ""')
  if [ -n "$resp" ]; then
    printf '%s  smoke:       inference works (%d tokens, "%s")\n' "$(green)" "$(echo "$reply" | jq -r '.eval_count // 0')" "$(echo "$resp" | head -c 40)"
  else
    printf '%s  smoke:       /api/generate returned no .response field\n' "$(red)"
    exit 3
  fi
fi

echo "" && echo "verdict: OK — remote Ollama at $HOST is ready"
exit 0
