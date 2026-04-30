#!/usr/bin/env bash
# scripts/ollama-system-check.sh — verify the host can run a given Ollama
# model before pulling it. Prints green/yellow/red status and exits:
#   0  OK    — system meets the model's recommended requirements
#   1  WARN  — system is below recommended; model will work but slowly
#   2  FAIL  — system is below the hard floor; pull would waste bandwidth
#
# Usage:
#   bash scripts/ollama-system-check.sh                        # default model
#   bash scripts/ollama-system-check.sh --model NAME           # explicit
#   CRABCC_OLLAMA_MODEL=… bash scripts/ollama-system-check.sh  # via env
#
# Checks:
#   - Platform (macOS arm64 / linux x86_64 / linux aarch64; warns on others)
#   - Total RAM (sysctl on macOS, /proc/meminfo on Linux)
#   - Free disk in OLLAMA_MODELS dir (default ~/.ollama/models)
#   - Ollama daemon reachable at $OLLAMA_HOST (default 127.0.0.1:11434)
#
# Per-model requirements table (q4_K_M @ 16k ctx, based on observed
# loads from the Ollama community + model card claims):
#
#   model                                          disk   ram   notes
#   voytas26/openclaw-oss-20b-deterministic        13 GB  16 GB OpenClaw default; gpt-oss:20b
#   voytas26/openclaw-qwen3vl-8b-opt                5 GB   8 GB lower-memory OpenClaw fallback
#   64500165/openclaw-omnicoder-2                   6 GB  10 GB long-loop tool calls
#   qwen2.5-coder:7b                               4.5 GB  6 GB generic code, no agent tuning
#   qwen2.5-coder:32b                              20 GB  24 GB high quality, slower
#
# Unknown models default to a safe minimum (8 GB disk, 12 GB RAM) and
# print a yellow note.
set -euo pipefail

MODEL="${CRABCC_OLLAMA_MODEL:-voytas26/openclaw-oss-20b-deterministic}"

while [ $# -gt 0 ]; do
  case "$1" in
    --model) MODEL="$2"; shift 2 ;;
    -h|--help) sed -n '1,30p' "$0" | sed 's/^# \?//'; exit 0 ;;
    *) echo "system-check: unknown arg $1" >&2; exit 2 ;;
  esac
done

# ── per-model requirements (GB) ──────────────────────────────────────
disk_req=8     # safe defaults for an unknown model
ram_req=12
known=0
case "$MODEL" in
  voytas26/openclaw-oss-20b-deterministic*)   disk_req=13; ram_req=16; known=1 ;;
  voytas26/openclaw-qwen3vl-8b-opt*)          disk_req=5;  ram_req=8;  known=1 ;;
  64500165/openclaw-omnicoder-2*)             disk_req=6;  ram_req=10; known=1 ;;
  qwen2.5-coder:7b*)                          disk_req=5;  ram_req=6;  known=1 ;;
  qwen2.5-coder:32b*)                         disk_req=20; ram_req=24; known=1 ;;
  qwen3*32b*|qwen3-coder*30b*)                disk_req=20; ram_req=24; known=1 ;;
esac

# ── platform / arch ──────────────────────────────────────────────────
os="$(uname -s)"
arch="$(uname -m)"
arch_status="green"
case "$os/$arch" in
  Darwin/arm64)              arch_label="macOS arm64 (Metal-accelerated, ideal)" ;;
  Darwin/x86_64)             arch_label="macOS x86_64 (works, no Metal — slower)";  arch_status="yellow" ;;
  Linux/x86_64)              arch_label="Linux x86_64 (CUDA if NVIDIA present, else CPU)" ;;
  Linux/aarch64)             arch_label="Linux aarch64 (CPU only unless ROCm/Mali)" ;;
  *)                         arch_label="$os/$arch (untested)";                      arch_status="yellow" ;;
esac

# ── total RAM (GB, integer) ─────────────────────────────────────────
case "$os" in
  Darwin)  ram_bytes=$(sysctl -n hw.memsize) ;;
  Linux)   ram_bytes=$(awk '/MemTotal/ {printf "%d", $2*1024}' /proc/meminfo) ;;
  *)       ram_bytes=0 ;;
esac
ram_gb=$(( ram_bytes / 1024 / 1024 / 1024 ))

# ── free disk in ollama models dir ──────────────────────────────────
models_dir="${OLLAMA_MODELS:-$HOME/.ollama/models}"
mkdir -p "$models_dir" 2>/dev/null || true
if [ -d "$models_dir" ]; then
  # df -k → 1024-byte blocks. Posix portable across macOS/linux.
  disk_free_gb=$(df -k "$models_dir" 2>/dev/null | awk 'NR==2 {printf "%d", $4/1024/1024}')
else
  disk_free_gb=0
fi

# ── ollama daemon reachable? ────────────────────────────────────────
host="${OLLAMA_HOST:-http://127.0.0.1:11434}"
if curl -fsS --max-time 3 "$host/api/version" >/dev/null 2>&1; then
  daemon_status="green"
  daemon_label="reachable at $host"
else
  daemon_status="red"
  daemon_label="NOT reachable at $host (run \`ollama serve\` or open the desktop app)"
fi

# ── verdict per check ───────────────────────────────────────────────
ram_status="green";  [ "$ram_gb"       -lt "$ram_req"  ] && ram_status="yellow"
[ "$ram_gb"       -lt $((ram_req  - 4)) ] && ram_status="red"
disk_status="green"; [ "$disk_free_gb" -lt "$disk_req" ] && disk_status="red"

# ── render ──────────────────────────────────────────────────────────
status_glyph() { case "$1" in green) echo "✓";; yellow) echo "⚠";; red) echo "✗";; esac; }
status_color() { case "$1" in green) echo $'\033[32m';; yellow) echo $'\033[33m';; red) echo $'\033[31m';; esac; }
RESET=$'\033[0m'

if [ -t 1 ]; then COLOR=1; else COLOR=0; fi
sg() { [ "$COLOR" = "1" ] && echo -n "$(status_color "$1")"; status_glyph "$1"; [ "$COLOR" = "1" ] && echo -n "$RESET"; }

echo "── ollama system check ──"
echo "model:         $MODEL$([ "$known" = "0" ] && echo "  (requirements unknown — using safe defaults)" || echo "")"
echo "$(sg "$arch_status")  arch:        $arch_label"
echo "$(sg "$ram_status")  RAM:         $ram_gb GB total · need ~$ram_req GB for this model"
echo "$(sg "$disk_status")  disk:        $disk_free_gb GB free in $models_dir · need ~$disk_req GB"
echo "$(sg "$daemon_status")  daemon:      $daemon_label"

# ── exit code ───────────────────────────────────────────────────────
worst=0
for s in "$arch_status" "$ram_status" "$disk_status" "$daemon_status"; do
  case "$s" in
    yellow) [ "$worst" -lt 1 ] && worst=1 ;;
    red)    worst=2 ;;
  esac
done

case "$worst" in
  0) echo "" && echo "verdict: OK — proceed with \`ollama pull $MODEL\`";          exit 0 ;;
  1) echo "" && echo "verdict: WARN — pull will work but performance may suffer"; exit 1 ;;
  2) echo "" && echo "verdict: FAIL — fix the red items before pulling";          exit 2 ;;
esac
