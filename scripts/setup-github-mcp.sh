#!/usr/bin/env bash
# scripts/setup-github-mcp.sh
#
# Bulletproof setup for the GitHub MCP server (remote + local Docker).
#
# What this does:
#   1. Reads a GitHub token from `gh auth token`.
#   2. Writes `env.GITHUB_PERSONAL_ACCESS_TOKEN` into ~/.claude/settings.local.json
#      (atomic, jq-merge — preserves all other keys).
#   3. Pulls ghcr.io/github/github-mcp-server:latest.
#   4. Registers a second MCP entry called `github-local` that runs the server
#      via Docker stdio.
#
# Idempotent: safe to re-run. Token is chmod 600.
# Re-run with FORCE=1 to re-register the local MCP even if already present.

set -euo pipefail

readonly SETTINGS_LOCAL="${HOME}/.claude/settings.local.json"
readonly LOCAL_MCP_NAME="github-local"
readonly LOCAL_DOCKER_IMAGE="ghcr.io/github/github-mcp-server:latest"
readonly REMOTE_URL="https://api.githubcopilot.com/mcp/"

c_blue='\033[1;34m'; c_yel='\033[1;33m'; c_red='\033[1;31m'; c_grn='\033[1;32m'; c_off='\033[0m'
log()  { printf "${c_blue}[setup]${c_off} %s\n" "$*" >&2; }
ok()   { printf "${c_grn}[ ok ]${c_off} %s\n" "$*" >&2; }
warn() { printf "${c_yel}[warn]${c_off} %s\n" "$*" >&2; }
err()  { printf "${c_red}[err ]${c_off} %s\n" "$*" >&2; }
die()  { err "$*"; exit 1; }

require() {
  command -v "$1" >/dev/null 2>&1 || die "missing dependency: '$1'${2:+ — $2}"
}

# ── 1. dependency check ─────────────────────────────────────────────────────
require gh     "brew install gh && gh auth login"
require jq     "brew install jq"
require claude "Claude Code CLI not on PATH (re-install Claude Code)"
require docker "install Docker Desktop, OR use Apple's 'container' runtime + alias docker='container'"

# ── 2. fetch token ──────────────────────────────────────────────────────────
log "fetching gh CLI token"
GH_TOKEN="$(gh auth token 2>/dev/null || true)"
[[ -n "${GH_TOKEN:-}" ]] || die "gh auth token returned nothing — run: gh auth login"
case "$GH_TOKEN" in
  gho_*|ghp_*|github_pat_*) ok "got GitHub token (prefix=${GH_TOKEN:0:4})" ;;
  *) warn "unexpected token prefix '${GH_TOKEN:0:4}…' — proceeding anyway" ;;
esac

# ── 3. merge into ~/.claude/settings.local.json ────────────────────────────
log "writing GITHUB_PERSONAL_ACCESS_TOKEN → ${SETTINGS_LOCAL}"
mkdir -p "$(dirname "$SETTINGS_LOCAL")"
[[ -s "$SETTINGS_LOCAL" ]] || echo '{}' > "$SETTINGS_LOCAL"

# Validate existing JSON; bail rather than corrupt it.
jq -e . "$SETTINGS_LOCAL" >/dev/null \
  || die "${SETTINGS_LOCAL} is not valid JSON — fix it before re-running"

tmp="$(mktemp "${TMPDIR:-/tmp}/cc-settings.XXXXXX")"
trap 'rm -f "$tmp"' EXIT

jq --arg t "$GH_TOKEN" \
  '.env = ((.env // {}) + {GITHUB_PERSONAL_ACCESS_TOKEN: $t})' \
  "$SETTINGS_LOCAL" > "$tmp"

chmod 600 "$tmp"
mv "$tmp" "$SETTINGS_LOCAL"
chmod 600 "$SETTINGS_LOCAL"
trap - EXIT
ok "settings.local.json updated (mode 600)"

# ── 4. pull docker image ────────────────────────────────────────────────────
log "pulling ${LOCAL_DOCKER_IMAGE}"
docker pull "$LOCAL_DOCKER_IMAGE" >/dev/null
ok "image ready"

# ── 5. register local MCP ───────────────────────────────────────────────────
already_registered() {
  claude mcp list 2>/dev/null | awk -F: '{print $1}' | grep -qx "$LOCAL_MCP_NAME"
}

if already_registered && [[ -z "${FORCE:-}" ]]; then
  ok "MCP '${LOCAL_MCP_NAME}' already registered (FORCE=1 to re-register)"
else
  if already_registered; then
    log "FORCE=1 — removing existing '${LOCAL_MCP_NAME}'"
    claude mcp remove "$LOCAL_MCP_NAME" >/dev/null 2>&1 || true
  fi
  log "registering local MCP '${LOCAL_MCP_NAME}'"
  claude mcp add "$LOCAL_MCP_NAME" -- \
    docker run -i --rm \
      -e GITHUB_PERSONAL_ACCESS_TOKEN \
      "$LOCAL_DOCKER_IMAGE"
  ok "registered '${LOCAL_MCP_NAME}'"
fi

# ── 6. summary ──────────────────────────────────────────────────────────────
cat >&2 <<EOF

──────────────────────────────────────────────────────────────────────
✓ setup complete

Remote MCP (existing):  plugin:github:github  →  ${REMOTE_URL}
Local  MCP (new):       ${LOCAL_MCP_NAME}             →  docker stdio

next steps:
  1) RESTART Claude Code (env vars are read at startup):
       /exit  (then re-launch claude)

  2) verify both connect:
       claude mcp list
     expect '✓ Connected' for both 'plugin:github:github' and '${LOCAL_MCP_NAME}'

  3) run the JSON-RPC test against both:
       scripts/test-github-mcp.sh
──────────────────────────────────────────────────────────────────────
EOF
