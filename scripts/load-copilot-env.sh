#!/usr/bin/env bash
# Load GitHub Environment "copilot" secrets onto this machine (and optionally m3).
#
# GitHub does not expose secret values via `gh secret list`. This script:
#   1. Dispatches .github/workflows/load-copilot-env.yml
#   2. Downloads the private artifact copilot.env
#   3. Installs to ~/.config/crabcc/copilot.env (mode 600)
#   4. Optionally copies to m3 and wires gh auth + ~/.zshrc
#
# Usage:
#   ./scripts/load-copilot-env.sh
#   ./scripts/load-copilot-env.sh --target m3
#   ./scripts/load-copilot-env.sh --out ~/.config/crabcc/copilot.env

set -euo pipefail

REPO="${GITHUB_REPOSITORY:-peterlodri-sec/crabcc}"
OUT="${HOME}/.config/crabcc/copilot.env"
TARGET=""
REF="${LOAD_COPILOT_ENV_REF:-main}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target) TARGET="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --ref) REF="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,12p' "$0"
      exit 0
      ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

command -v gh >/dev/null || { echo "gh required" >&2; exit 10; }
gh auth status >/dev/null 2>&1 || { echo "gh not authenticated — run: gh auth login" >&2; exit 10; }

mkdir -p "$(dirname "$OUT")"
chmod 700 "$(dirname "$OUT")" 2>/dev/null || true

# Capture timestamp before dispatch so the poll can ignore pre-existing runs.
DISPATCH_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "Dispatching load-copilot-env.yml on $REPO @ $REF ..."
gh workflow run load-copilot-env.yml -R "$REPO" --ref "$REF"

# Exponential backoff: wait for a run created at or after DISPATCH_TS.
# Without the timestamp filter, `gh run list --limit 1` can grab an older
# run that was already queued, leading to a download of stale artifacts.
RUN_ID=""
for delay in 3 6 12 24 48; do
    sleep "$delay"
    RUN_ID="$(gh run list -R "$REPO" --workflow=load-copilot-env.yml \
        --limit 5 --json databaseId,createdAt \
        -q "[.[] | select(.createdAt >= \"$DISPATCH_TS\")] | .[0].databaseId // empty" \
        2>/dev/null || true)"
    [[ -n "$RUN_ID" && "$RUN_ID" != "null" ]] && break
done
[[ -n "$RUN_ID" && "$RUN_ID" != "null" ]] \
    || { echo "timed out waiting for workflow run (dispatched at $DISPATCH_TS)" >&2; exit 11; }

echo "Waiting for run $RUN_ID ..."
# `timeout` is GNU coreutils; not present on macOS by default.
if command -v timeout >/dev/null 2>&1; then
    timeout 300 gh run watch "$RUN_ID" -R "$REPO" --exit-status
else
    gh run watch "$RUN_ID" -R "$REPO" --exit-status
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
gh run download "$RUN_ID" -R "$REPO" -D "$TMP" -n copilot-env
[[ -f "$TMP/copilot.env" ]] || { echo "artifact missing copilot.env" >&2; exit 12; }

install -m 600 "$TMP/copilot.env" "$OUT"
echo "Installed $OUT"

# gh on this machine (optional refresh)
if grep -q '^export GH_PERSONAL_TOKEN=' "$OUT"; then
  # shellcheck disable=SC1090
  source "$OUT"
  if [[ -n "${GH_PERSONAL_TOKEN:-}" ]]; then
    printf '%s' "$GH_PERSONAL_TOKEN" | gh auth login --with-token 2>/dev/null \
      && echo "gh auth refreshed from GH_PERSONAL_TOKEN"
  fi
fi

install_remote() {
  local host="$1"
  echo "Installing on $host ..."
  ssh "$host" 'mkdir -p ~/.config/crabcc && chmod 700 ~/.config/crabcc'
  scp "$OUT" "${host}:~/.config/crabcc/copilot.env"
  ssh "$host" 'chmod 600 ~/.config/crabcc/copilot.env'
  ssh "$host" 'grep -q "copilot.env" ~/.zshrc 2>/dev/null || echo "source ~/.config/crabcc/copilot.env" >> ~/.zshrc'
  ssh "$host" 'zsh -lic "
    source ~/.config/crabcc/copilot.env
    if [[ -n \"\${GH_PERSONAL_TOKEN:-}\" ]]; then
      printf \"%s\" \"\$GH_PERSONAL_TOKEN\" | gh auth login --with-token
    fi
    command -v opencode >/dev/null && opencode --version || true
    gh auth status 2>&1 | head -3
    for v in OPENROUTER_API_KEY LINEAR_API_KEY GH_PERSONAL_TOKEN; do
      if [[ -n \"\${!v:-}\" ]]; then echo \"\$v=set\"; else echo \"\$v=missing\"; fi
    done
  "'
}

if [[ -n "$TARGET" ]]; then
  install_remote "$TARGET"
fi
