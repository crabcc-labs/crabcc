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

# Start happy in daemon mode. Backgrounded so post-create doesn't block.
# Logs to /tmp/happy.log; survives container start. post-start.sh re-checks
# on every resume in case the process was reaped during sleep.
if command -v happy >/dev/null 2>&1; then
  echo "[post-create] starting happy --daemon (log: /tmp/happy.log)"
  nohup happy --daemon >/tmp/happy.log 2>&1 &
fi

echo "[post-create] done — try: task --list"
