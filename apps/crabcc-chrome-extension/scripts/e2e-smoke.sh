#!/usr/bin/env bash
# Local-only smoke: launches a clean Chrome profile with the unpacked
# extension and verifies the offscreen tether handshakes against
# `crabcc serve` on :7878. NOT run in CI.
#
# Prereqs: `crabcc serve` already running on :7878 in another terminal,
# Chrome / Chromium installed.
set -euo pipefail

EXT_DIR="$(cd "$(dirname "$0")/.." && pwd)/dist"
PROFILE_DIR="${TMPDIR:-/tmp}/crabcc-chrome-e2e-$$"

if [[ ! -d "$EXT_DIR" ]]; then
  echo "[e2e] dist/ missing — run \`task chrome:build\` first" >&2
  exit 1
fi

if ! curl -sf http://localhost:7878/api/health > /dev/null; then
  echo "[e2e] crabcc serve is not reachable on :7878 — start it first" >&2
  exit 1
fi

CHROME="${CHROME_BIN:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
if [[ ! -x "$CHROME" ]]; then
  echo "[e2e] Chrome not found at $CHROME (override with CHROME_BIN=…)" >&2
  exit 1
fi

echo "[e2e] launching Chrome → profile=$PROFILE_DIR ext=$EXT_DIR"
"$CHROME" \
  --user-data-dir="$PROFILE_DIR" \
  --load-extension="$EXT_DIR" \
  --no-first-run \
  --no-default-browser-check \
  about:blank
