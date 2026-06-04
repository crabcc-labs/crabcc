#!/usr/bin/env bash
# bench-flow-matrix.sh — the complete crabcc token benchmark.
#
# Two tables, both "vanilla shell vs the full crabcc rewrite flow" (engine
# rewrite -> RTK -> Morph, plus cat->read / cat->dasel / media), measured on
# a clean `git archive HEAD` tree (no target/ noise), tokens = bytes/4:
#   * per agent profile  — claude_code / nullclaw / zeroclaw command mixes
#     (mirrors crates/crabcc-mcp/benches/agent_profiles.rs)
#   * per operation      — the individual rewrite rules
#
# Modes:
#   (default)     print the full matrix to stdout
#   --readme      splice it into README.md between the BENCH markers,
#                 stamped with the commit ref + UTC timestamp + host.
#
# OpenRouter lane (opt-in: OPENROUTER_API_KEY + MODELS set) appends a
# real-tokenizer prompt_tokens table. Off by default (costs real tokens).
#
# Env: REPO (default git toplevel), CRABCC (default target/release|debug),
#      MORPH_API_KEY (engages the Morph stage when set).
set -euo pipefail

REPO="${REPO:-$(git rev-parse --show-toplevel)}"
if [ -n "${CRABCC:-}" ]; then :; elif [ -x "$REPO/target/release/crabcc" ]; then CRABCC="$REPO/target/release/crabcc";
elif [ -x "$REPO/target/debug/crabcc" ]; then CRABCC="$REPO/target/debug/crabcc";
else CRABCC="$(command -v crabcc || true)"; fi
[ -n "$CRABCC" ] || { echo "no crabcc binary (build first: task build)" >&2; exit 1; }
crabcc_dir="$(cd "$(dirname "$CRABCC")" && pwd)"
export PATH="$crabcc_dir:$PATH"
command -v jq >/dev/null || { echo "jq required" >&2; exit 1; }

README="$REPO/README.md"
MODE="${1:-stdout}"

# Clean source tree (no target/) for reproducibility.
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
git -C "$REPO" archive HEAD | tar -x -C "$WORK"
( cd "$WORK" && crabcc index >/dev/null 2>&1 )

if [ -n "${MORPH_API_KEY:-}" ] && [ -z "${CRABCC_NO_MORPH:-}" ]; then MORPH=ON; else MORPH=off; fi
COMMIT="$(git -C "$REPO" rev-parse --short=12 HEAD)"
STAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
HOST="$(uname -sm)"

tok() { echo $(( ${1:-0} / 4 )); }

# Rewrite `$1` through the flow; echo the wrapped command (empty = no rewrite).
flow_of() {
  crabcc shell rewrite --command "$1" 2>/dev/null \
    | jq -r '.hookSpecificOutput.updatedInput.command // empty'
}
# "vanilla_tok flow_tok" for a single-shot command, run in $WORK.
measure() {
  local cmd="$1" van flow wrapped
  van=$( cd "$WORK" && bash -c "$cmd" 2>/dev/null | wc -c )
  wrapped=$( flow_of "$cmd" )
  if [ -n "$wrapped" ]; then flow=$( cd "$WORK" && bash -c "$wrapped" 2>/dev/null | wc -c ); else flow="$van"; fi
  echo "$(tok "$van") $(tok "$flow")"
}
# Signed reduction: "-97%" = 97% fewer tokens; "+38%" = grew (tiny outputs).
pct() {
  [ "$1" -gt 0 ] 2>/dev/null || { echo "n/a"; return; }
  awk "BEGIN{r=(1-$2/$1)*100; printf (r>=0?\"-%.0f%%\":\"+%.0f%%\"), (r>=0?r:-r)}"
}

# Pick real, representative (largest) files from the clean tree.
SRC="crates/crabcc-core/src/store.rs"; [ -f "$WORK/$SRC" ] || SRC="$(cd "$WORK" && find crates -name '*.rs' | head -1)"
largest() { ( cd "$WORK" && find . \( "$@" \) -not -path './target/*' -type f -exec wc -c {} + 2>/dev/null \
  | awk '$2!="total"{print}' | sort -n | tail -1 | awk '{print $2}' | sed 's|^\./||' ); }
JSON="$(largest -name '*.json')"
YAML="$(largest -name '*.yaml' -o -name '*.yml')"

# ── per-profile ──────────────────────────────────────────────────────────
profile_row() {
  local name="$1"; shift; local vsum=0 fsum=0 v f
  for c in "$@"; do read -r v f <<<"$(measure "$c")"; vsum=$((vsum+v)); fsum=$((fsum+f)); done
  printf '| %-12s | %8d | %8d | %7s |\n' "$name" "$vsum" "$fsum" "$(pct "$vsum" "$fsum")"
}
claude_code=("grep -rn Store ." "cat $SRC" "find . -name '*.rs'" "grep -rn Backend ." "grep -rn 'pub fn' .")
nullclaw=("grep -rn Store ." "grep -rn Backend ." "grep -rn Rewrite .")
zeroclaw=("grep -rn Store ." "grep -rn Backend ." "find . -name '*.rs'")
[ -n "$JSON" ] && claude_code+=("cat $JSON")

PROF_TABLE="$(
  echo '| profile | vanilla | flow | reduction |'
  echo '|---|---:|---:|---:|'
  profile_row claude_code "${claude_code[@]}"
  profile_row nullclaw    "${nullclaw[@]}"
  profile_row zeroclaw    "${zeroclaw[@]}"
)"

# ── per-operation ────────────────────────────────────────────────────────
op_row() { # label cmd
  local v f; read -r v f <<<"$(measure "$2")"
  printf '| %s | %d | %d | %s |\n' "$1" "$v" "$f" "$(pct "$v" "$f")"
}
# read-source re-read: warm the session cache, then measure the 2nd read (stub).
readsrc_row() {
  local sid="bench-$$" van flow w
  van=$( cd "$WORK" && cat "$SRC" 2>/dev/null | wc -c )
  w=$( flow_of "cat $SRC" )
  if [ -n "$w" ]; then
    ( cd "$WORK" && CRABCC_SESSION_ID="$sid" bash -c "$w" >/dev/null 2>&1 )
    flow=$( cd "$WORK" && CRABCC_SESSION_ID="$sid" bash -c "$w" 2>/dev/null | wc -c )
  else flow="$van"; fi
  printf '| read source (re-read) | %d | %d | %s |\n' "$(tok "$van")" "$(tok "$flow")" "$(pct "$(tok "$van")" "$(tok "$flow")")"
}
OP_TABLE="$(
  echo '| operation | vanilla | flow | reduction |'
  echo '|---|---:|---:|---:|'
  op_row 'find symbol (grep Store)' "grep -rn Store ."
  op_row 'find refs (grep Backend)' "grep -rn Backend ."
  op_row 'list files (find *.rs)'   "find . -name '*.rs'"
  op_row 'text search (grep pub fn)' "grep -rn 'pub fn' ."
  [ -n "$JSON" ] && op_row 'read JSON (cat *.json)' "cat $JSON"
  [ -n "$YAML" ] && command -v dasel >/dev/null && op_row 'read YAML (cat *.yaml)' "cat $YAML"
  readsrc_row
)"

BLOCK="$(cat <<EOF
_Auto-generated by \`scripts/bench-flow-matrix.sh\` — commit \`$COMMIT\`, $STAMP, $HOST. Morph stage: $MORPH. Tokens = bytes/4, clean \`git archive\` tree._

**Per agent profile** (vanilla shell vs the full crabcc rewrite flow):

$PROF_TABLE

**Per operation:**

$OP_TABLE

Reproduce: \`task bench-flow-matrix\` (or \`scripts/bench-flow-matrix.sh\`). Full methodology: [\`docs/PERF-648\`](./docs/PERF-648-agent-shell-and-deps.md).
EOF
)"

# ── OpenRouter lane (opt-in) ─────────────────────────────────────────────
if [ -n "${OPENROUTER_API_KEY:-}" ] && [ -n "${MODELS:-}" ]; then
  van_ctx="$WORK/.van"; flow_ctx="$WORK/.flow"; : >"$van_ctx"; : >"$flow_ctx"
  for c in "${claude_code[@]}"; do
    ( cd "$WORK" && bash -c "$c" 2>/dev/null ) >>"$van_ctx" || true
    w=$( flow_of "$c" ); if [ -n "$w" ]; then ( cd "$WORK" && bash -c "$w" 2>/dev/null ) >>"$flow_ctx" || true
    else ( cd "$WORK" && bash -c "$c" 2>/dev/null ) >>"$flow_ctx" || true; fi
  done
  ptoks() {
    jq -n --arg m "$1" --rawfile ctx "$2" \
      '{model:$m,max_tokens:1,messages:[{role:"user",content:("Reply OK.\n\nContext:\n"+$ctx)}]}' \
    | curl -s -m 60 https://openrouter.ai/api/v1/chat/completions \
        -H "Authorization: Bearer $OPENROUTER_API_KEY" -H "Content-Type: application/json" --data @- \
    | jq -r '.usage.prompt_tokens // "err"'
  }
  OR_TABLE="$(echo '| model | vanilla ptok | flow ptok | reduction |'; echo '|---|---:|---:|---:|'
    for m in $MODELS; do v=$(ptoks "$m" "$van_ctx"); f=$(ptoks "$m" "$flow_ctx"); printf '| %s | %s | %s | %s |\n' "$m" "$v" "$f" "$(pct "$v" "$f" 2>/dev/null)"; done)"
  BLOCK="$BLOCK

**OpenRouter real-tokenizer lane:**

$OR_TABLE"
fi

# ── emit ─────────────────────────────────────────────────────────────────
if [ "$MODE" = "--readme" ]; then
  BLOCK="$BLOCK" python3 - "$README" <<'PY'
import os, re, sys
path = sys.argv[1]
block = os.environ["BLOCK"]
text = open(path).read()
start, end = "<!-- BENCH:START -->", "<!-- BENCH:END -->"
new = f"{start}\n{block}\n{end}"
if start in text and end in text:
    text = re.sub(re.escape(start) + r".*?" + re.escape(end), lambda _: new, text, flags=re.S)
else:
    raise SystemExit(f"markers {start}/{end} not found in {path}")
open(path, "w").write(text)
print(f"updated {path} between BENCH markers")
PY
else
  echo "$BLOCK"
fi
