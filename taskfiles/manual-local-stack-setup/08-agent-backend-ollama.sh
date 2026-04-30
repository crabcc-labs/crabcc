#!/usr/bin/env bash
# Stage 8 — `crabcc agent --backend ollama` (default since v2.8.x).
. "$(dirname "$0")/lib.sh"

section "8. agent --backend ollama"

# Dry-run — no docker required.
out=$(crabcc agent --run "list functions in lib.rs" --backend ollama --dry-run --no-refresh --no-repomix 2>&1)
echo "$out" | grep -q "ollama/qwen2.5-coder" && pass "dry-run banner shows qwen2.5-coder default" \
    || fail "default model not surfaced"

# Real run with stack already up.
real=$(crabcc agent --run "ping" --backend ollama --no-refresh --no-repomix 2>&1)
echo "$real" | grep -qE "(ollama stack ready|crabcc agent: id=)" && pass "real run launched" \
    || fail "real run didn't show launch banner"

# meta.json carries backend + model.
last=$(ls -1t "$HOME/.crabcc/agents/" 2>/dev/null | head -1)
if [[ -n "$last" ]] && [[ -f "$HOME/.crabcc/agents/$last/meta.json" ]]; then
    backend=$(jq -r '.backend // empty' "$HOME/.crabcc/agents/$last/meta.json")
    model=$(jq -r '.model // empty' "$HOME/.crabcc/agents/$last/meta.json")
    [[ "$backend" == "ollama" ]] && pass "meta.json backend=ollama" \
        || fail "meta.json backend=$backend (expected ollama)"
    [[ "$model" =~ qwen2.5-coder ]] && pass "meta.json model contains qwen2.5-coder" \
        || warn "meta.json model=$model (acceptable if --model overrode)"
else
    fail "no run dir under ~/.crabcc/agents/"
fi

report
