#!/usr/bin/env bash
# bench-flow-matrix.sh — the "with vs without crabcc hooks" token matrix.
#
# Deterministic lane (always): replays a representative shell-command mix
# for each agent profile (claude_code / nullclaw / zeroclaw) twice —
# vanilla (raw grep/cat/find) and through the full crabcc flow (the exact
# `crabcc shell rewrite` pipeline: engine rewrite -> RTK, plus the
# cat->read and media paths) — and reports tokens (bytes/4) per profile.
# Runs against a clean `git archive` tree so `target/` noise can't skew it
# (matches docs/PERF-648 methodology); fully reproducible, no network.
#
# OpenRouter lane (opt-in: OPENROUTER_API_KEY set): for each model in
# MODELS, sends the same task with vanilla vs flow-compressed context and
# reports the API's real `usage.prompt_tokens` per model — the only
# model-dependent number (different tokenizers), since the byte reductions
# above are model-independent. Costs real tokens; off unless you opt in.
#
# Usage:
#   scripts/bench-flow-matrix.sh                      # deterministic only
#   OPENROUTER_API_KEY=... MODELS="anthropic/claude-haiku-4-5 deepseek/deepseek-chat" \
#     scripts/bench-flow-matrix.sh                    # + model lane
#
# Env:
#   REPO     repo to bench (default: git toplevel of cwd)
#   CRABCC   crabcc binary (default: target/release/crabcc, else target/debug, else PATH)
set -euo pipefail

REPO="${REPO:-$(git rev-parse --show-toplevel)}"
# Resolve the crabcc binary; its dir goes FIRST on PATH so the wrapped
# flow commands (which call bare `crabcc lookup/read/morph`) resolve to the
# build under test, not a stale installed version.
if [ -n "${CRABCC:-}" ]; then :; elif [ -x "$REPO/target/release/crabcc" ]; then CRABCC="$REPO/target/release/crabcc";
elif [ -x "$REPO/target/debug/crabcc" ]; then CRABCC="$REPO/target/debug/crabcc";
else CRABCC="$(command -v crabcc || true)"; fi
[ -n "$CRABCC" ] || { echo "no crabcc binary (build first: task build)" >&2; exit 1; }
crabcc_dir="$(cd "$(dirname "$CRABCC")" && pwd)"
export PATH="$crabcc_dir:$PATH"
command -v jq >/dev/null || { echo "jq required" >&2; exit 1; }

# Clean source tree (no target/) for reproducibility.
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
git -C "$REPO" archive HEAD | tar -x -C "$WORK"
( cd "$WORK" && crabcc index >/dev/null 2>&1 )

tok() { echo $(( ${1:-0} / 4 )); }   # token ~= bytes/4 (perf-648 convention)

# Pick a real, tracked source + json file from the clean tree for cat tests.
SRC="$(cd "$WORK" && git -C "$REPO" ls-files 'crates/crabcc-core/src/store.rs' | head -1)"
[ -n "$SRC" ] || SRC="$(cd "$WORK" && find crates -name '*.rs' | head -1)"
JSON="$(cd "$WORK" && find . -name '*.json' -not -path './target/*' | head -1 | sed 's|^\./||')"

# Profiles: representative command mixes grounded in benches/agent_profiles.rs.
claude_code=("grep -rn Store ." "cat $SRC" "find . -name '*.rs'" "grep -rn Backend ." "grep -rn 'pub fn' .")
nullclaw=("grep -rn Store ." "grep -rn Backend ." "grep -rn Rewrite .")
zeroclaw=("grep -rn Store ." "grep -rn Backend ." "find . -name '*.rs'")
[ -n "$JSON" ] && claude_code+=("cat $JSON")

# Run one command vanilla + through the flow in $WORK; echo "van_tok flow_tok".
measure() {
  local cmd="$1" van flow wrapped
  van=$( cd "$WORK" && bash -c "$cmd" 2>/dev/null | wc -c )
  wrapped=$( crabcc shell rewrite --command "$cmd" --cwd "$WORK" 2>/dev/null \
             | jq -r '.hookSpecificOutput.updatedInput.command // empty' )
  if [ -n "$wrapped" ]; then
    flow=$( cd "$WORK" && bash -c "$wrapped" 2>/dev/null | wc -c )
  else
    flow="$van"   # no rewrite -> identical to vanilla
  fi
  echo "$(tok "$van") $(tok "$flow")"
}

profile_row() {
  local name="$1"; shift
  local -a cmds=("$@")
  local vsum=0 fsum=0
  for c in "${cmds[@]}"; do
    read -r v f <<<"$(measure "$c")"
    vsum=$((vsum + v)); fsum=$((fsum + f))
  done
  local pct="n/a"
  [ "$vsum" -gt 0 ] && pct="$(awk "BEGIN{printf \"-%.0f%%\", (1-$fsum/$vsum)*100}")"
  printf '| %-12s | %9d | %9d | %7s |\n' "$name" "$vsum" "$fsum" "$pct"
}

echo "# crabcc flow token matrix"
echo
echo "Clean tree: \`git archive HEAD\` of \`$(basename "$REPO")\`. crabcc: \`$CRABCC\`."
echo "tokens = bytes/4."
echo
echo "| profile      | vanilla   | flow      | reduct  |"
echo "|--------------|-----------|-----------|---------|"
profile_row claude_code "${claude_code[@]}"
profile_row nullclaw    "${nullclaw[@]}"
profile_row zeroclaw    "${zeroclaw[@]}"

# ── OpenRouter lane (opt-in) ────────────────────────────────────────────
if [ -n "${OPENROUTER_API_KEY:-}" ] && [ -n "${MODELS:-}" ]; then
  echo
  echo "## OpenRouter prompt-token lane (real tokenizers)"
  echo
  # Build vanilla vs flow context blobs from the claude_code mix.
  van_ctx="$WORK/.van.ctx"; flow_ctx="$WORK/.flow.ctx"; : >"$van_ctx"; : >"$flow_ctx"
  for c in "${claude_code[@]}"; do
    ( cd "$WORK" && bash -c "$c" 2>/dev/null ) >>"$van_ctx" || true
    w=$( crabcc shell rewrite --command "$c" 2>/dev/null | jq -r '.hookSpecificOutput.updatedInput.command // empty' )
    if [ -n "$w" ]; then ( cd "$WORK" && bash -c "$w" 2>/dev/null ) >>"$flow_ctx" || true
    else ( cd "$WORK" && bash -c "$c" 2>/dev/null ) >>"$flow_ctx" || true; fi
  done
  ptoks() { # ptoks <model> <ctx-file> -> usage.prompt_tokens (or "err")
    local model="$1" ctx="$2"
    jq -n --arg m "$model" --rawfile ctx "$ctx" \
      '{model:$m, max_tokens:1, messages:[{role:"user", content:("Reply OK.\n\nContext:\n"+$ctx)}]}' \
    | curl -s -m 60 https://openrouter.ai/api/v1/chat/completions \
        -H "Authorization: Bearer $OPENROUTER_API_KEY" \
        -H "Content-Type: application/json" --data @- \
    | jq -r '.usage.prompt_tokens // "err"'
  }
  echo "| model | vanilla ptok | flow ptok | reduct |"
  echo "|-------|--------------|-----------|--------|"
  for m in $MODELS; do
    v=$(ptoks "$m" "$van_ctx"); f=$(ptoks "$m" "$flow_ctx")
    pct="n/a"
    case "$v$f" in *err*) : ;; *) [ "$v" -gt 0 ] 2>/dev/null && pct="$(awk "BEGIN{printf \"-%.0f%%\", (1-$f/$v)*100}")";; esac
    printf '| %s | %s | %s | %s |\n' "$m" "$v" "$f" "$pct"
  done
fi
