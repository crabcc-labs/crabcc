#!/usr/bin/env bash
# Stage 3 — bring the Compose stack up + verify health + labels.
. "$(dirname "$0")/lib.sh"

section "3. Stack up"

cd install/ollama-stack
docker compose up -d --wait >/dev/null 2>&1 \
    && pass "compose up --wait" \
    || fail "compose up failed"

n_healthy=$(docker compose ps --format json 2>/dev/null \
    | jq -r 'select(.Health=="healthy") | .Name' | wc -l | tr -d ' ')
[[ "$n_healthy" -ge 3 ]] && pass "$n_healthy services healthy" \
    || fail "expected ≥3 healthy services, got $n_healthy"

# Labels — issue #105 verification.
ids=$(docker compose ps -q)
if [[ -n "$ids" ]]; then
    labels=$(docker inspect $ids --format '{{json .Config.Labels}}' 2>/dev/null \
            | tr -d '\n')
    echo "$labels" | grep -q 'com.crabcc.project' && pass "com.crabcc labels present" \
        || fail "com.crabcc labels missing"
fi

cd - >/dev/null

report
