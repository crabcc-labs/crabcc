#!/usr/bin/env bash
# Runs every time the container starts after creation.
set -euo pipefail

echo "[post-create] warming cargo + building crabcc"

# Build the workspace once so rust-analyzer is responsive immediately.
cargo build --workspace --all-features || true

# Install the crabcc binary onto PATH so `crabcc sym <name>` works in the shell.
# Uses the release profile (same as `task install`) for a stable, fast binary.
cargo install --locked --path crates/crabcc-cli || true

# Bootstrap the symbol index for the repo.
if command -v crabcc >/dev/null 2>&1; then
  crabcc index || true
fi

echo "[post-create] done — try: task --list"
