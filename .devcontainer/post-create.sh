#!/usr/bin/env bash
# Runs every time the container starts after creation.
set -euo pipefail

echo "[post-create] warming cargo + building crabcc"

# Build the workspace once so rust-analyzer is responsive immediately.
cargo build --workspace --all-features || true

# Build the index so `crabcc sym <name>` works in the new shell.
if command -v crabcc >/dev/null 2>&1; then
  crabcc index || true
fi

echo "[post-create] done — try: task --list"
