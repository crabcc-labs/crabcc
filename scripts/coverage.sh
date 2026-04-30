#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/coverage.sh
#
# Thin wrapper around `cargo llvm-cov` that picks the right output format
# and lands the artifact under .summary/coverage/. Keeps the Taskfile
# entry one line, side-steps a YAML block-scalar / heredoc parse quirk.
#
# Usage:
#   scripts/coverage.sh                  # html (default)
#   scripts/coverage.sh html | lcov | json | text
#   scripts/coverage.sh text             # console summary only (fast)
#
# CHANGELOG
#   v1.0.0 (2026-04-30) — initial cut.
# ---------------------------------------------------------------------------

set -euo pipefail

FORMAT="${1:-html}"
OUT=".summary/coverage"
mkdir -p "$OUT"

case "$FORMAT" in
    html)
        cargo llvm-cov --workspace --html --output-dir "$OUT"
        echo
        echo "report → $OUT/html/index.html"
        ;;
    lcov)
        cargo llvm-cov --workspace --lcov --output-path "$OUT/lcov.info"
        echo "report → $OUT/lcov.info"
        ;;
    json)
        cargo llvm-cov --workspace --json --output-path "$OUT/coverage.json"
        echo "report → $OUT/coverage.json"
        ;;
    text|--summary-only|summary)
        cargo llvm-cov --workspace --summary-only
        ;;
    *)
        echo "FORMAT must be one of: html | lcov | text | json (got: $FORMAT)" >&2
        exit 1
        ;;
esac
