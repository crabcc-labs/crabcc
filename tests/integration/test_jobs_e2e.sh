#!/usr/bin/env bash
# tests/integration/test_jobs_e2e.sh — BullMQ jobs e2e integration test.
#
# Starts the jobs stack (Redis + jobs-worker) via Apple Containers or Docker,
# then exercises the full round-trip:
#   1. crabcc jobs list          — baseline depths
#   2. crabcc jobs submit        — enqueue agent:run + repo:index jobs
#   3. crabcc jobs status        — check queued state
#   4. worker picks up           — wait for completion
#   5. crabcc jobs status        — verify completed / unknown (pruned)
#   6. crabcc jobs cancel        — cancel a delayed job
#   7. Bull Board healthcheck    — GET :3001 returns 200 (if dev-tools up)
#
# Usage:
#   bash tests/integration/test_jobs_e2e.sh
#   SKIP_STACK=1 bash ...       # assume stack already running
#   SKIP_STACK=1 BULL_BOARD=1 bash ...
#
# Exit: 0 pass · 1 fail · 2 prerequisites missing
set -euo pipefail

PASS=0; FAIL=0; ERRORS=()

ok()   { PASS=$((PASS+1)); printf "  \033[32m✓\033[0m %s\n" "$*"; }
fail() { FAIL=$((FAIL+1)); ERRORS+=("$*"); printf "  \033[31m✗\033[0m %s\n" "$*"; }
info() { printf "  \033[34m▶\033[0m %s\n" "$*"; }

CRABCC="${CRABCC:-crabcc}"
SKIP_STACK="${SKIP_STACK:-0}"
BULL_BOARD="${BULL_BOARD:-0}"
REDIS_URL="${REDIS_URL:-redis://127.0.0.1:6379}"

# ── 0. Prerequisites ──────────────────────────────────────────────────────────
echo "Jobs e2e integration test"
echo ""

command -v "$CRABCC" >/dev/null || { echo "crabcc not on PATH — task install"; exit 2; }
command -v redis-cli  >/dev/null || { echo "redis-cli missing — brew install redis"; exit 2; }
command -v python3    >/dev/null || { echo "python3 missing"; exit 2; }

# ── 1. Stack startup ──────────────────────────────────────────────────────────
COMPOSE_CMD=""
if command -v container &>/dev/null; then
    COMPOSE_CMD="container compose"
elif command -v docker &>/dev/null; then
    COMPOSE_CMD="docker compose"
fi

if [ "$SKIP_STACK" = "0" ] && [ -n "$COMPOSE_CMD" ]; then
    info "Starting jobs stack (${COMPOSE_CMD%%[ ]*} compose --profile jobs)..."
    PROFILES="--profile jobs"
    [ "$BULL_BOARD" = "1" ] && PROFILES="$PROFILES --profile dev-tools"
    $COMPOSE_CMD -f install/dev/docker-compose.yml $PROFILES up -d --build --wait 2>/dev/null
    trap '$COMPOSE_CMD -f install/dev/docker-compose.yml --profile jobs --profile dev-tools down 2>/dev/null || true' EXIT
    ok "stack up via $COMPOSE_CMD"
fi

# Wait for Redis to be ready
for i in $(seq 1 10); do
    redis-cli -u "$REDIS_URL" ping 2>/dev/null | grep -q PONG && break
    sleep 1
done
redis-cli -u "$REDIS_URL" ping 2>/dev/null | grep -q PONG && ok "Redis reachable" || { fail "Redis not reachable at $REDIS_URL"; exit 1; }

# ── 2. Baseline list ──────────────────────────────────────────────────────────
echo ""
echo "── list ──────────────────────────────────────────────────────────────────"

LIST=$("$CRABCC" jobs list 2>/dev/null)
HAS_QUEUES=$(printf '%s' "$LIST" | python3 -c "
import sys,json
d=json.load(sys.stdin)
q=d.get('queues',{})
print('yes' if 'agent:run' in q else 'no')
" 2>/dev/null)
[ "$HAS_QUEUES" = "yes" ] && ok "jobs list returns agent:run queue" || fail "jobs list missing expected queues: $LIST"

# ── 3. Submit agent:run ───────────────────────────────────────────────────────
echo ""
echo "── submit ────────────────────────────────────────────────────────────────"

SUB=$("$CRABCC" jobs submit \
    --queue agent:run \
    --name  "e2e-test-$(date +%s)" \
    --data  '{"prompt":"respond with the word PONG and nothing else"}' \
    2>/dev/null)

JOB_ID=$(printf '%s' "$SUB" | python3 -c "import sys,json; print(json.load(sys.stdin).get('id',''))" 2>/dev/null)
[ -n "$JOB_ID" ] && ok "submitted agent:run → job_id=$JOB_ID" || fail "submit returned no id: $SUB"

# ── 4. Status check — should be queued ───────────────────────────────────────
echo ""
echo "── status (queued) ───────────────────────────────────────────────────────"

if [ -n "$JOB_ID" ]; then
    STATUS=$("$CRABCC" jobs status --queue agent:run --id "$JOB_ID" 2>/dev/null)
    S=$(printf '%s' "$STATUS" | python3 -c "import sys,json; print(json.load(sys.stdin).get('status',''))" 2>/dev/null)
    [ "$S" = "queued" ] || [ "$S" = "active" ] && ok "job $JOB_ID is $S" || fail "expected queued/active, got: $S"
fi

# ── 5. Submit a delayed job, then cancel it ───────────────────────────────────
echo ""
echo "── submit delayed + cancel ───────────────────────────────────────────────"

DELAYED=$("$CRABCC" jobs submit \
    --queue agent:run \
    --name  "e2e-delayed-$(date +%s)" \
    --data  '{"prompt":"delayed test"}' \
    --delay-ms 60000 \
    2>/dev/null)
DEL_ID=$(printf '%s' "$DELAYED" | python3 -c "import sys,json; print(json.load(sys.stdin).get('id',''))" 2>/dev/null)
[ -n "$DEL_ID" ] && ok "submitted delayed job → id=$DEL_ID" || fail "delayed submit failed: $DELAYED"

if [ -n "$DEL_ID" ]; then
    CANCEL=$("$CRABCC" jobs cancel --queue agent:run --id "$DEL_ID" 2>/dev/null)
    C_OK=$(printf '%s' "$CANCEL" | python3 -c "import sys,json; print('yes' if json.load(sys.stdin).get('ok') else 'no')" 2>/dev/null)
    [ "$C_OK" = "yes" ] && ok "cancelled delayed job $DEL_ID" || fail "cancel failed: $CANCEL"
fi

# ── 6. Submit repo:index job ──────────────────────────────────────────────────
echo ""
echo "── repo:index ────────────────────────────────────────────────────────────"

IDX=$("$CRABCC" jobs submit --queue repo:index --name "e2e-index" --data '{}' 2>/dev/null)
IDX_ID=$(printf '%s' "$IDX" | python3 -c "import sys,json; print(json.load(sys.stdin).get('id',''))" 2>/dev/null)
[ -n "$IDX_ID" ] && ok "submitted repo:index → id=$IDX_ID" || fail "repo:index submit failed: $IDX"

# ── 7. Wait for completion (up to 30 s) ──────────────────────────────────────
echo ""
echo "── wait for completion ───────────────────────────────────────────────────"

if [ -n "$JOB_ID" ]; then
    COMPLETED=0
    for i in $(seq 1 30); do
        sleep 1
        S=$("$CRABCC" jobs status --queue agent:run --id "$JOB_ID" 2>/dev/null | \
            python3 -c "import sys,json; print(json.load(sys.stdin).get('status',''))" 2>/dev/null)
        if [ "$S" = "completed" ] || [ "$S" = "unknown" ]; then
            COMPLETED=1
            ok "job $JOB_ID reached state: $S"
            break
        fi
    done
    [ "$COMPLETED" -eq 1 ] || fail "job $JOB_ID did not complete within 30 s (last status: $S)"
fi

# ── 8. Bull Board healthcheck ─────────────────────────────────────────────────
if [ "$BULL_BOARD" = "1" ]; then
    echo ""
    echo "── bull board ────────────────────────────────────────────────────────────"
    BB_CODE=$(curl -so /dev/null -w "%{http_code}" "http://localhost:${BULL_BOARD_PORT:-3001}/" 2>/dev/null)
    [ "$BB_CODE" = "200" ] && ok "Bull Board :${BULL_BOARD_PORT:-3001} → HTTP $BB_CODE" || fail "Bull Board returned HTTP $BB_CODE (expected 200)"
fi

# ── summary ───────────────────────────────────────────────────────────────────
echo ""
echo "── summary ───────────────────────────────────────────────────────────────"
echo "  passed: $PASS   failed: $FAIL"
if [ ${#ERRORS[@]} -gt 0 ]; then
    for e in "${ERRORS[@]}"; do echo "    ✗ $e"; done
fi
[ "$FAIL" -eq 0 ]
