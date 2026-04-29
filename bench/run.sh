#!/usr/bin/env bash
# crabcc benchmark harness — token-cost A/B between raw tools and crabcc.
#
# Plan (TODO):
#   1. Drive Claude Code via `claude -p` against a fixture repo.
#   2. Run a fixed task list twice: once with crabcc skill enabled,
#      once with it disabled (force grep/Read path).
#   3. Capture per-turn input/output token counts from the JSON transcript.
#   4. Emit savings table (reuses pkg-cache projection-table style).
set -euo pipefail
echo "TODO: wire bench harness"
