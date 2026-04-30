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
body=$(jq -nc --arg m "$MODEL" --argjson p "$FULL_PROMPT" \
  '{model:$m, prompt:$p, stream:false, format:"json",
    options:{num_predict:2048, temperature:0.2}}')

reply=$(curl -fsS --max-time "$TIMEOUT" \
              -H 'Content-Type: application/json' \
              -d "$body" \
              "$HOST/api/generate" 2>&1) || {
  echo "agent-runtime: /api/generate failed: $reply" >&2
  exit 2
}

raw_response=$(echo "$reply" | jq -r '.response // ""')
if [ -z "$raw_response" ]; then
  echo "agent-runtime: empty .response from Ollama" >&2
  exit 3
fi

# Try to parse the model's response as JSON; if it fails, surface the
# raw text so the caller can decide whether to retry with stronger
# prompting.
if parsed=$(echo "$raw_response" | jq -e . 2>/dev/null); then
  jq -n \
    --arg model "$MODEL" \
    --argjson parsed "$parsed" \
    --arg raw       "$raw_response" \
    '{ok:true, model:$model,
      tool_call: ($parsed.tool_call // null),
      final:     ($parsed.final     // null),
      thinking:  ($parsed.thinking  // null),
      raw: $raw}' > "$OUTPUT"
  echo "agent-runtime: ok · model=$MODEL · output=$OUTPUT" >&2
  exit 0
else
  jq -n --arg model "$MODEL" --arg raw "$raw_response" \
    '{ok:false, model:$model, error:"non-json reply", raw:$raw}' > "$OUTPUT"
  echo "agent-runtime: model returned non-JSON; raw written to $OUTPUT" >&2
  exit 3
fi
