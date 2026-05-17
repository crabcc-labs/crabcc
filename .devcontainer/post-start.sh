#!/usr/bin/env bash
# Runs every time the container resumes (including after codespace sleep).
# Kept fast — anything heavy belongs in on-create.sh or post-create.sh.
set -euo pipefail

# Sanity ping so the existing log line is preserved.
cargo --version
task --version 2>/dev/null || true

# Make sure happy is running. Codespace sleep may have reaped the daemon;
# this re-launches it cheaply (`pgrep` exits 1 → start; exits 0 → no-op).
if command -v happy >/dev/null 2>&1; then
  if ! pgrep -f "happy --daemon" >/dev/null 2>&1; then
    echo "[post-start] (re)starting happy --daemon (log: /tmp/happy.log)"
    nohup happy --daemon >>/tmp/happy.log 2>&1 &
  else
    echo "[post-start] happy --daemon already running"
  fi
fi
