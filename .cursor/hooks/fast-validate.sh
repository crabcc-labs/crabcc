#!/usr/bin/env bash
set -u

echo "[hook] Fast validation placeholder for Rust/TOML edits."
echo "[hook] Keeping this non-blocking and under 5s."

if command -v cargo >/dev/null 2>&1; then
  echo "[hook] Suggested optional command: cargo fmt --all -- --check"
else
  echo "[hook] cargo is not available; no project command executed."
fi

exit 0
