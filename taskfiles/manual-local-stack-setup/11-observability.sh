#!/usr/bin/env bash
# Stage 11 — observability spot-checks for the ollama_stack tracing surface.
. "$(dirname "$0")/lib.sh"

section "11. Observability"

# Re-up to capture fresh events.
crabcc ollama-stack down >/dev/null 2>&1
out=$(RUST_LOG=crabcc_core::ollama_stack=info crabcc ollama-stack up 2>&1)

for tag in "ollama_stack.detect" "ollama_stack.up.start" "ollama_stack.up.done" "ollama_stack.container_info"; do
    echo "$out" | grep -q "$tag" \
        && pass "event '$tag' emitted" \
        || fail "event '$tag' MISSING"
done

# request_id auto-population.
echo "$out" | grep -q "request_id=" \
    && pass "request_id field present" \
    || fail "request_id field missing on tracing events"

report
