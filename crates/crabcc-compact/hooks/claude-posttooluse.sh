#!/usr/bin/env bash
COMPACT="${CRABCC_COMPACT_BIN:-crabcc-compact}"
if ! command -v "$COMPACT" &>/dev/null; then exit 0; fi
input=$(cat)
printf '%s' "$input" | "$COMPACT" posttooluse 2>/dev/null
