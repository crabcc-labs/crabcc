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
MODEL="${CRABCC_OLLAMA_MODEL:-voytas26/openclaw-oss-20b-deterministic}"
HOST="${OLLAMA_HOST:-http://127.0.0.1:11434}"
TIMEOUT=600

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
TOOLS=$(jq -c '.tools // []' "$TASK")

# The model gets a strict contract: respond with JSON only, one of two
# shapes. This matches the OpenClaw tool-call training; the
# `format: "json"` Ollama option then guarantees parse-able JSON.
read -r -d '' CONTRACT <<'EOF' || true
You are a tool-using agent. Respond with EXACTLY one JSON object, no prose, no commentary.
Choose ONE of these two shapes:

  { "tool_call": { "name": "<tool_name>", "args": { /* tool-specific */ } } }

OR

  { "final": "<final answer string>" }

You MAY include an optional "thinking" field with concise reasoning (≤ 200 words).
Do NOT emit both tool_call and final in the same reply.
Pick a tool from the supplied catalogue or emit final.
EOF

FULL_PROMPT=$(jq -n \
  --arg sys      "$SYS"     \
  --arg contract "$CONTRACT" \
  --argjson tools "$TOOLS"   \
  --arg user     "$USR"      \
  '"# System\n" + $sys +
   "\n\n# Contract\n" + $contract +
   "\n\n# Tools\n" + ($tools | tostring) +
   "\n\n# User\n" + $user')

# ── call /api/generate ───────────────────────────────────────────────
# NOTE: do NOT set format:"json". Thinking + tool-calling models
# (gpt-oss, OpenClaw, Qwen3-Thinking) emit structured output via the
# native .tool_calls + .thinking fields; format:"json" actively clips
# them to empty strings. Verified empirically against
# voytas26/openclaw-oss-20b-deterministic on 2026-04-30.
body=$(jq -nc --arg m "$MODEL" --argjson p "$FULL_PROMPT" \
  '{model:$m, prompt:$p, stream:false,
    options:{num_predict:2048, temperature:0.2}}')

reply=$(curl -fsS --max-time "$TIMEOUT" \
              -H 'Content-Type: application/json' \
              -d "$body" \
              "$HOST/api/generate" 2>&1) || {
  echo "agent-runtime: /api/generate failed: $reply" >&2
  exit 2
}

# Drop the giant .context array before any further processing so jq
# stays cheap on big replies.
reply_trim=$(echo "$reply" | jq -c 'del(.context)')

# Three places the model can put its output:
#   .response   — free-form text (final answer; "" when tool-calling)
#   .thinking   — reasoning trace (gpt-oss / OpenClaw native field)
#   .tool_calls — structured tool calls Ollama auto-extracts; shape:
#                 [ {function: {name, arguments, index?}} ]
response=$( echo "$reply_trim" | jq -r '.response // ""')
thinking=$( echo "$reply_trim" | jq -r '.thinking // ""')
tool_calls_raw=$(echo "$reply_trim" | jq -c '.tool_calls // []')
first_call=$(echo "$tool_calls_raw" | jq -c '.[0] // null')

# Normalize the first tool call into our contract shape: {name, args}.
# Ollama's native shape nests under .function; the contract flattens it
# so downstream parsers don't need to care about provider quirks.
if [ "$first_call" != "null" ]; then
  tool_call=$(echo "$first_call" | jq -c '
    if   .function then {name: .function.name, args: (.function.arguments // {})}
    elif .name     then {name: .name,          args: (.arguments // .args // {})}
    else null end')
else
  tool_call="null"
fi

# Decide ok/error. The model satisfied the contract iff it produced
# either a tool_call OR non-empty final text.
if [ "$tool_call" != "null" ] || [ -n "$response" ]; then
  jq -n \
    --arg model       "$MODEL" \
    --argjson tc      "$tool_call" \
    --arg final       "$response" \
    --arg thinking    "$thinking" \
    --argjson raw     "$reply_trim" \
    '{ok:true, model:$model,
      tool_call: $tc,
      final:     (if $final == "" then null else $final end),
      thinking:  (if $thinking == "" then null else $thinking end),
      raw: $raw}' > "$OUTPUT"
  echo "agent-runtime: ok · model=$MODEL · output=$OUTPUT" >&2
  echo "agent-runtime: $(if [ "$tool_call" != "null" ]; then echo "tool_call=$(echo "$tool_call" | jq -r .name)"; else echo "final=$(echo "$response" | head -c 60)…"; fi)" >&2
  exit 0
else
  # Neither tool_call nor final — surface every field for diagnosis.
  jq -n \
    --arg model      "$MODEL" \
    --argjson raw    "$reply_trim" \
    --arg thinking   "$thinking" \
    '{ok:false, model:$model,
      error:"empty response and no tool_calls (model only thought)",
      thinking: (if $thinking == "" then null else $thinking end),
      raw: $raw}' > "$OUTPUT"
  echo "agent-runtime: model produced no actionable output (thinking only); raw written to $OUTPUT" >&2
  exit 3
fi
