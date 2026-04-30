#!/usr/bin/env bash
# scripts/ollama-fanout.sh — parallel sub-agent dispatch via local Ollama.
#
# Replaces Claude Code's `Agent` tool fan-out for the *-audit skills with
# a free, local, Apple-Silicon-Metal-accelerated alternative. Each input
# prompt is POSTed to /api/generate in parallel; the merged JSON output
# matches the contract those skills expect ({agent,findings,…}).
#
# Inputs:
#   --prompts FILE        path to a JSON file: [{"name":"A","prompt":"…"}, …]
#                         OR JSONL (one object per line).
#   --output FILE         path to merged JSON output (overwritten).
#   --model NAME          Ollama tag. Default: $CRABCC_OLLAMA_MODEL or
#                         voytas26/openclaw-oss-20b-deterministic (OpenClaw-tuned
#                         Qwen3-VL 8B: JSON tool calls, <thinking>
#                         reasoning, 16k ctx, fits 8 GB VRAM on Apple
#                         Silicon — battle-tested for autonomous-agent
#                         JSON output; the audit skills emit JSON
#                         findings, so this tuning matches).
#                         Other recommended OpenClaw variants:
#                           voytas26/openclaw-oss-20b-deterministic
#                             (gpt-oss:20b, deterministic, 32+ GB)
#                           64500165/openclaw-omnicoder-2
#                             (Qwen3.5-9B, stable in long agent loops)
#                         Smaller fallback (no agent tuning, generic):
#                           qwen2.5-coder:7b
#   --parallel N          max concurrent requests. Default: 4 (matches
#                         the audit skills' cluster count).
#   --host URL            Ollama base URL. Default: $OLLAMA_HOST or
#                         http://127.0.0.1:11434
#   --timeout SECS        per-request timeout. Default: 600 (10 min for
#                         long context-window completions on 7B models).
#   --json-mode           ask the model for JSON output via the `format:
#                         "json"` Ollama option. Recommended for the
#                         audit skills.
#   --no-pull             skip the auto `ollama pull <model>` if missing.
#
# Exit codes:
#   0   all prompts completed (individual ok/error inside the merged JSON)
#   1   bad arguments / missing tooling
#   2   Ollama daemon not reachable
#   3   model not present and --no-pull set
#
# Examples:
#   bash scripts/ollama-fanout.sh \
#     --prompts /tmp/warp-prompts.json \
#     --output  /tmp/warp-replies.json \
#     --json-mode
#
#   # use the deterministic gpt-oss:20b OpenClaw variant for higher quality
#   CRABCC_OLLAMA_MODEL=voytas26/openclaw-oss-20b-deterministic \
#     bash scripts/ollama-fanout.sh --prompts … --output … --json-mode
set -euo pipefail

# ── arg parse ─────────────────────────────────────────────────────────
PROMPTS=""
OUTPUT=""
MODEL="${CRABCC_OLLAMA_MODEL:-voytas26/openclaw-oss-20b-deterministic}"
PARALLEL=4
HOST="${OLLAMA_HOST:-http://127.0.0.1:11434}"
TIMEOUT=600
JSON_MODE=0
NO_PULL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --prompts)   PROMPTS="$2"; shift 2 ;;
    --output)    OUTPUT="$2"; shift 2 ;;
    --model)     MODEL="$2"; shift 2 ;;
    --parallel)  PARALLEL="$2"; shift 2 ;;
    --host)      HOST="$2"; shift 2 ;;
    --timeout)   TIMEOUT="$2"; shift 2 ;;
    --json-mode) JSON_MODE=1; shift ;;
    --no-pull)   NO_PULL=1; shift ;;
    -h|--help)
      sed -n '1,40p' "$0" | sed 's/^# \?//'
      exit 0
      ;;
    *)
      echo "ollama-fanout: unknown arg: $1" >&2
      exit 1
      ;;
  esac
done

[ -n "$PROMPTS" ] || { echo "ollama-fanout: --prompts required" >&2; exit 1; }
[ -n "$OUTPUT"  ] || { echo "ollama-fanout: --output required"  >&2; exit 1; }
[ -f "$PROMPTS" ] || { echo "ollama-fanout: prompts file not found: $PROMPTS" >&2; exit 1; }

for tool in curl jq; do
  command -v "$tool" >/dev/null || { echo "ollama-fanout: missing tool: $tool" >&2; exit 1; }
done

# ── pre-flight: system check (advisory) ───────────────────────────────
# Re-uses scripts/ollama-system-check.sh when present; soft-fails on WARN
# (exit 1), hard-fails on FAIL (exit 2). Skip with --skip-system-check.
SYS_CHECK="$(dirname "$0")/ollama-system-check.sh"
if [ "${SKIP_SYSTEM_CHECK:-0}" != "1" ] && [ -x "$SYS_CHECK" ]; then
  if CRABCC_OLLAMA_MODEL="$MODEL" "$SYS_CHECK" >&2; then
    :  # green
  else
    rc=$?
    case "$rc" in
      1) echo "ollama-fanout: system check returned WARN — continuing" >&2 ;;
      2) echo "ollama-fanout: system check FAILED — refusing to fan out" >&2; exit 2 ;;
    esac
  fi
fi

# ── pre-flight: daemon + model ────────────────────────────────────────
if ! curl -fsS --max-time 5 "$HOST/api/version" >/dev/null 2>&1; then
  echo "ollama-fanout: cannot reach Ollama at $HOST" >&2
  echo "  start it with:  ollama serve  (or run the desktop app)" >&2
  echo "  install:        brew install ollama  (macOS)" >&2
  exit 2
fi

# Ensure the model is present locally; pull on demand unless --no-pull.
if ! curl -fsS --max-time 5 "$HOST/api/tags" 2>/dev/null \
      | jq -e --arg m "$MODEL" '.models[]? | select(.name == $m)' >/dev/null; then
  if [ "$NO_PULL" = "1" ]; then
    echo "ollama-fanout: model $MODEL not present and --no-pull set" >&2
    exit 3
  fi
  echo "ollama-fanout: pulling $MODEL (one-time, ~4–25 GB depending on tag)…" >&2
  if ! command -v ollama >/dev/null; then
    echo "  ollama CLI not on PATH; the model isn't local. Install ollama or pre-pull." >&2
    exit 3
  fi
  ollama pull "$MODEL" >&2
fi

# ── normalize input → JSONL of {name,prompt} ──────────────────────────
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

# Accept either a JSON array or JSONL. Split into one file per prompt
# so xargs -P can stream results back independently.
if jq -e 'type == "array"' "$PROMPTS" >/dev/null 2>&1; then
  jq -c '.[]' "$PROMPTS" > "$TMPDIR/prompts.jsonl"
else
  cp "$PROMPTS" "$TMPDIR/prompts.jsonl"
fi

# Index prompts by line number → file. Each worker reads its own slot.
nl -ba "$TMPDIR/prompts.jsonl" | while read -r idx line; do
  printf '%s\n' "$line" > "$TMPDIR/p-$idx.json"
done

# ── per-prompt worker ────────────────────────────────────────────────
worker() {
  local pfile="$1"
  local idx
  idx="$(basename "$pfile" .json)"
  idx="${idx#p-}"
  local name prompt body reply
  name="$(jq -r '.name'   "$pfile")"
  prompt="$(jq -r '.prompt' "$pfile")"

  if [ "$JSON_MODE" = "1" ]; then
    body="$(jq -nc --arg m "$MODEL" --arg p "$prompt" \
      '{model:$m, prompt:$p, stream:false, format:"json", options:{temperature:0.2}}')"
  else
    body="$(jq -nc --arg m "$MODEL" --arg p "$prompt" \
      '{model:$m, prompt:$p, stream:false, options:{temperature:0.2}}')"
  fi

  reply="$(curl -fsS --max-time "$TIMEOUT" \
                 -H 'Content-Type: application/json' \
                 -d "$body" \
                 "$HOST/api/generate" 2>"$TMPDIR/err-$idx.txt" || true)"

  if [ -z "$reply" ]; then
    jq -nc --arg name "$name" --arg err "$(cat "$TMPDIR/err-$idx.txt" 2>/dev/null || true)" \
      '{name:$name, ok:false, error:($err // "empty reply")}' > "$TMPDIR/r-$idx.json"
    return
  fi

  # Ollama returns {response: "...", done: true, …}. We pass the inner
  # response through verbatim (the audit skills' aggregator parses it).
  jq -nc --arg name "$name" --argjson reply "$reply" \
    '{name:$name, ok:true, response:$reply.response, model:$reply.model,
      eval_count:$reply.eval_count, eval_duration:$reply.eval_duration}' \
    > "$TMPDIR/r-$idx.json"
}

export -f worker
export TMPDIR HOST MODEL TIMEOUT JSON_MODE

# ── fan out (xargs -P keeps it portable; no GNU parallel dep) ────────
ls "$TMPDIR"/p-*.json | xargs -n1 -P "$PARALLEL" -I{} bash -c 'worker "$@"' _ {}

# ── merge results in input order ─────────────────────────────────────
jq -s '.' "$TMPDIR"/r-*.json > "$OUTPUT"

# ── stderr summary so the orchestrator can decide quickly ────────────
total=$(jq 'length' "$OUTPUT")
ok=$(jq '[.[] | select(.ok == true)] | length' "$OUTPUT")
echo "ollama-fanout: $ok/$total ok · model=$MODEL · host=$HOST · output=$OUTPUT" >&2
