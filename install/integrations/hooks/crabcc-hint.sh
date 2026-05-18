#!/usr/bin/env bash
# Cursor beforeShellExecution hook — nudge symbol lookups toward crabcc.
# Installed by: crabcc setup install-integrations --target cursor --project
set -euo pipefail
cmd="${CURSOR_SHELL_COMMAND:-${1:-}}"
if [[ -z "$cmd" ]] && [[ -t 0 ]]; then
  cmd="$(cat 2>/dev/null || true)"
fi
if [[ -n "$cmd" ]] && command -v crabcc >/dev/null 2>&1; then
  if echo "$cmd" | grep -qE '(^| )(rg|grep( -[a-zA-Z]+)?)\s+[A-Za-z_][A-Za-z0-9_]+|(^| )find\s+[^|]*-name\b'; then
    echo 'hint: try crabcc sym/refs/callers — symbol-aware and ~10x cheaper on tokens' >&2
  fi
fi
exit 0
