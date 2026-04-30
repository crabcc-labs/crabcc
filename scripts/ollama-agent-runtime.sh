#!/usr/bin/env bash
# scripts/ollama-agent-runtime.sh — minimal Ollama-backed agent runtime.
# Used by `crabcc agent --run` (PR #69) when the user passes
# `--llm ollama` (or sets CRABCC_AGENT_LLM=ollama). Wraps the local OR
# network-exposed Ollama daemon so the agent runtime is decoupled from
# Claude Code / API providers.
#
# This is the building block — a single-shot completion with a tool-call
# JSON contract. The full multi-turn loop (tool execution + reply) lives
# in crabcc-cli/src/agent_runtime.rs and shells out to this script per
# turn.
#
# Inputs (env or flags):
#   --task FILE        path to a JSON file with the agent's input:
#                        { "system": "...", "user": "...",
#                          "tools": [{"name":"crabcc.fuzzy",
#                                     "description":"…",
#                                     "schema":{…}}, …] }
#   --output FILE      path to the JSON reply (overwritten):
#                        { "ok": true,
#                          "tool_call":  {"name":"...","args":{…}}
#                                     | null,
#                          "final":      "...text..." | null,
#                          "thinking":   "..." | null,
#                          "raw":        "<model response>" }
#   --model NAME       override $CRABCC_OLLAMA_MODEL.
#   --host URL         override $OLLAMA_HOST.
#
# Behaviour:
#   - Builds a system prompt that embeds the tools list and instructs
#     the model to reply with strict JSON (one of: tool_call OR final).
#   - Calls /api/generate with format:"json", num_predict capped at
#     2048, temperature 0.2 (tighter than fan-out's 0.5 default —
#     audit & agent runtime want low variance).
#   - Pre-flight uses scripts/ollama-system-check.sh (local) or
#     scripts/ollama-network-check.sh (remote, when host != localhost).
#
# Exit codes:
#   0  ok            — wrote --output JSON
#   1  bad input     — --task file missing / malformed
#   2  daemon issue  — pre-flight failed
#   3  parse error   — model reply wasn't JSON-parseable as tool_call/final
set -euo pipefail

TASK=""
OUTPUT=""
MODEL="${CRABCC_OLLAMA_MODEL:-qwen3.5:35b-a3b-coding-nvfp4}"
HOST="${OLLAMA_HOST:-http://127.0.0.1:4000}"
TIMEOUT=600
# Qwen3.5-35B-A3B native context: 262144. Use full window; LiteLLM caps at 128k if OOM.
NUM_CTX="${OLLAMA_NUM_CTX:-262144}"
NUM_KEEP=512
API_KEY="${CRABCC_OLLAMA_API_KEY:-${LITELLM_MASTER_KEY:-ollama}}"

while [ $# -gt 0 ]; do
  case "$1" in
    --task)    TASK="$2"; shift 2 ;;
    --output)  OUTPUT="$2"; shift 2 ;;
    --model)   MODEL="$2"; shift 2 ;;
    --host)    HOST="$2"; shift 2 ;;
    --timeout) TIMEOUT="$2"; shift 2 ;;
    -h|--help) sed -n '1,40p' "$0" | sed 's/^# \?//'; exit 0 ;;
    *) echo "agent-runtime: unknown arg $1" >&2; exit 1 ;;
  esac
done

[ -n "$TASK" ]   || { echo "agent-runtime: --task required"   >&2; exit 1; }
[ -n "$OUTPUT" ] || { echo "agent-runtime: --output required" >&2; exit 1; }
[ -f "$TASK" ]   || { echo "agent-runtime: $TASK not found"   >&2; exit 1; }

for tool in curl jq; do
  command -v "$tool" >/dev/null || { echo "agent-runtime: missing tool: $tool" >&2; exit 1; }
done

# ── pre-flight: pick local vs network check based on host ────────────
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
case "$HOST" in
  http://127.0.0.1:*|http://localhost:*|http://[::1]:*)
    if [ -x "$SCRIPT_DIR/ollama-system-check.sh" ]; then
      CRABCC_OLLAMA_MODEL="$MODEL" "$SCRIPT_DIR/ollama-system-check.sh" >&2 || rc=$?
      [ "${rc:-0}" = "2" ] && exit 2
    fi
    ;;
  *)
    if [ -x "$SCRIPT_DIR/ollama-network-check.sh" ]; then
      OLLAMA_HOST="$HOST" CRABCC_OLLAMA_MODEL="$MODEL" \
        "$SCRIPT_DIR/ollama-network-check.sh" >&2 || rc=$?
      [ "${rc:-0}" = "2" ] && exit 2
    fi
    ;;
esac

# ── build the system prompt with the tool catalogue ──────────────────
SYS=$(jq -r '.system // ""' "$TASK")
USR=$(jq -r '.user'         "$TASK")
# Compact JSON — no whitespace waste in the context window.
TOOLS=$(jq -c '.tools // []' "$TASK")

# Build a cheatsheet section from available crabcc context on the host.
CRABCC_DIR="${HOME}/.crabcc"
CHEATSHEET=""
if [ -d "$CRABCC_DIR" ]; then
  REPOMIX_PATHS=$(find "$CRABCC_DIR" -name "*.xml" -newer "$CRABCC_DIR" -maxdepth 3 2>/dev/null \
    | head -5 | tr '\n' ' ' || true)
  HAS_GRAPH=$([ -f "$CRABCC_DIR/graph.json" ] && echo "yes" || echo "no")
  CHEATSHEET=$(cat <<SHEET
## Workspace Context
- crabcc index: $CRABCC_DIR (graph: $HAS_GRAPH, tantivy FTS, sessions/memory)
$([ -n "$REPOMIX_PATHS" ] && echo "- Repomix packs available: $REPOMIX_PATHS")

## Quick Lookup (call these via tool_call or shell-out)
| Goal | Command |
|------|---------|
| Symbol definition | crabcc sym <name> |
| Callers of a fn | crabcc callers <name> |
| Fuzzy search | crabcc fuzzy <query> |
| List module files | crabcc files <path> |
| References | crabcc refs <name> |
| Graph overview | crabcc graph |
| Memory read | crabcc memory get <key> |
SHEET
)
fi

# Strict JSON-only contract. /no_think suppresses Qwen3.5 thinking blocks;
# stop_words handles older builds that ignore the token.
CONTRACT='/no_think You are a tool-using agent. Reply with EXACTLY one JSON object, no prose.
Shape A (use a tool): {"tool_call":{"name":"<tool>","args":{}},"thinking":"<≤100 words>"}
Shape B (done):       {"final":"<answer>","thinking":"<≤100 words>"}
thinking is optional. Never emit both tool_call and final.'

FULL_PROMPT=$(jq -n \
  --arg sys       "$SYS"       \
  --arg cheat     "$CHEATSHEET" \
  --arg contract  "$CONTRACT"  \
  --argjson tools "$TOOLS"     \
  --arg user      "$USR"       \
  '(if $sys != "" then "# System\n" + $sys + "\n\n" else "" end) +
   (if $cheat != "" then $cheat + "\n\n" else "" end) +
   "# Contract\n" + $contract +
   "\n\n# Tools\n" + ($tools | tojson) +
   "\n\n# Task\n" + $user')

# ── call /v1/chat/completions via SSE ────────────────────────────────
# Routes through LiteLLM at HOST (default :4000) for prompt caching.
# Both LiteLLM and direct Ollama support OpenAI-compat chat completions.
MESSAGES=$(jq -cn \
  --arg sys  "$FULL_PROMPT" \
  --arg user "$USR" \
  '[{role:"system",content:$sys},{role:"user",content:$user}]')

body=$(jq -nc \
  --arg     m    "$MODEL"    \
  --argjson msgs "$MESSAGES" \
  --argjson ctx  "$NUM_CTX"  \
  '{model:$m, messages:$msgs, stream:true, temperature:0.2, max_tokens:4096,
    options:{num_ctx:$ctx, num_keep:'"$NUM_KEEP"'}}')

# Accumulate SSE stream ("data: {...}" lines; stop on "[DONE]").
FULL_CONTENT=""
while IFS= read -r sse_line; do
  [[ "$sse_line" == "data: [DONE]" ]] && break
  [[ "$sse_line" != data:* ]] && continue
  content=$(printf '%s' "${sse_line#data: }" | \
    jq -r '.choices[0].delta.content // empty' 2>/dev/null || true)
  [ -n "$content" ] && FULL_CONTENT+="$content"
done < <(curl -fsS -N --max-time "$TIMEOUT" \
              -H 'Content-Type: application/json' \
              -H "Authorization: Bearer $API_KEY" \
              -d "$body" \
              "$HOST/v1/chat/completions") || {
  echo "agent-runtime: /v1/chat/completions failed (HOST=$HOST MODEL=$MODEL)" >&2
  exit 2
}

# Strip Qwen3.5 <think>…</think> blocks in case /no_think was ignored.
response=$(printf '%s' "$FULL_CONTENT" | \
  perl -0777 -pe 's|<think>.*?</think>\s*||gs' 2>/dev/null || \
  printf '%s' "$FULL_CONTENT")
thinking=$(printf '%s' "$response" | jq -r '.thinking // empty' 2>/dev/null || true)
tool_call=$(printf '%s' "$response" | jq -c '.tool_call // null' 2>/dev/null || echo "null")

# Extract final text from the parsed JSON contract (Shape B).
final_text=$(printf '%s' "$response" | jq -r '.final // empty' 2>/dev/null || true)
# Fallback: if response isn't valid JSON, treat the whole thing as final.
if [ -z "$final_text" ] && [ "$tool_call" = "null" ] && [ -n "$response" ]; then
  final_text="$response"
fi

# Decide ok/error. The model satisfied the contract iff it produced
# either a tool_call OR non-empty final text.
if [ "$tool_call" != "null" ] || [ -n "$final_text" ]; then
  jq -n \
    --arg model       "$MODEL" \
    --argjson tc      "$tool_call" \
    --arg final       "$final_text" \
    --arg thinking    "$thinking" \
    --arg raw         "$FULL_CONTENT" \
    '{ok:true, model:$model,
      tool_call: $tc,
      final:     (if $final == "" then null else $final end),
      thinking:  (if $thinking == "" then null else $thinking end),
      raw: $raw}' > "$OUTPUT"
  echo "agent-runtime: ok · model=$MODEL · output=$OUTPUT" >&2
  echo "agent-runtime: $(if [ "$tool_call" != "null" ]; then echo "tool_call=$(printf '%s' "$tool_call" | jq -r .name)"; else echo "final=$(printf '%s' "$final_text" | head -c 60)…"; fi)" >&2
  exit 0
else
  jq -n \
    --arg model     "$MODEL" \
    --arg raw       "$FULL_CONTENT" \
    --arg thinking  "$thinking" \
    '{ok:false, model:$model,
      error:"empty response and no tool_call (model only thought or returned nothing)",
      thinking: (if $thinking == "" then null else $thinking end),
      raw: $raw}' > "$OUTPUT"
  echo "agent-runtime: model produced no actionable output; raw written to $OUTPUT" >&2
  exit 3
fi
