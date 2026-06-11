#!/usr/bin/env bash
# Pool status + queue depth on m3 (via SSH forward for :8080 health).
set -euo pipefail

REMOTE="${M3_POOL_REMOTE:-m3-task}"
LB_URL="${M3_HEALTH_URL:-http://127.0.0.1:8080}"

if ! curl -fsS --max-time 2 "${LB_URL}/health" >/dev/null 2>&1; then
  ssh -fN m3 2>/dev/null || true
fi

echo "=== m3 pool (remote) ==="
ssh "$REMOTE" 'cd /opt/plodri/llama-server && ./lb/pool.sh status' 2>&1 || true

echo
echo "=== LB queue (local forward) ==="
curl -fsS --max-time 3 "${LB_URL}/lb/status" 2>/dev/null | jq . || echo "(lb unreachable — run: ssh -fN m3)"

echo
echo "=== opencode jobs on m3 ==="
ssh "$REMOTE" "ps aux | grep -c '[o]pencode run'" 2>/dev/null | awk '{print "count:", $1}'
