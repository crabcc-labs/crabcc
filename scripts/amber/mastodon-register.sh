#!/usr/bin/env bash
# Interactive Mastodon app registration + OAuth flow for social.crabcc.app
# Eliminates the Preferences -> Development -> New Application click-through.
#
# Usage: bash scripts/amber/mastodon-register.sh
#
# Output: prints MASTODON_ACCESS_TOKEN=<token> — add to your .env or shell rc.
set -euo pipefail

INSTANCE="https://social.crabcc.app"
REDIRECT="urn:ietf:wg:oauth:2.0:oob"
SCOPE="write:statuses"

__die() { echo "error: $*" >&2; exit 1; }
__check_cmd() { command -v "$1" &>/dev/null || __die "required: $1"; }

__check_cmd curl
__check_cmd jq

# ---- banner ----
cat <<'EOF'
  mastodon-register — social.crabcc.app OAuth setup
  --------------------------------------------------
  Automates: Preferences -> Development -> New Application
  Scope: write:statuses
EOF
echo

# ---- step 1: app name ----
printf "App name [crabcc-cli]: "
read -r APP_NAME
APP_NAME="${APP_NAME:-crabcc-cli}"

# ---- step 2: register app ----
echo
echo "Registering '${APP_NAME}' with ${INSTANCE}..."

resp="$(curl -sf -X POST "${INSTANCE}/api/v1/apps" \
    -F "client_name=${APP_NAME}" \
    -F "redirect_uris=${REDIRECT}" \
    -F "scopes=${SCOPE}" \
    -F "website=https://crabcc.app")" || __die "POST /api/v1/apps failed — is ${INSTANCE} reachable?"

CLIENT_ID="$(echo "${resp}" | jq -r '.client_id')"
CLIENT_SECRET="$(echo "${resp}" | jq -r '.client_secret')"

[ "${CLIENT_ID}"     != "null" ] || __die "no client_id in response: ${resp}"
[ "${CLIENT_SECRET}" != "null" ] || __die "no client_secret in response: ${resp}"

echo "  client_id     = ${CLIENT_ID}"
echo "  client_secret = ${CLIENT_SECRET:0:8}…"

# ---- step 3: authorize ----
AUTH_URL="${INSTANCE}/oauth/authorize?client_id=${CLIENT_ID}&scope=${SCOPE}&redirect_uri=${REDIRECT}&response_type=code"

echo
echo "Open this URL in your browser and authorize the app:"
echo
echo "  ${AUTH_URL}"
echo

# Try to open the browser automatically.
if command -v open &>/dev/null; then
    open "${AUTH_URL}" 2>/dev/null || true
elif command -v xdg-open &>/dev/null; then
    xdg-open "${AUTH_URL}" 2>/dev/null || true
fi

printf "Paste the authorization code here: "
read -r AUTH_CODE
[ -n "${AUTH_CODE}" ] || __die "no authorization code provided"

# ---- step 4: exchange code for token ----
echo
echo "Exchanging code for access token..."

token_resp="$(curl -sf -X POST "${INSTANCE}/oauth/token" \
    -F "grant_type=authorization_code" \
    -F "client_id=${CLIENT_ID}" \
    -F "client_secret=${CLIENT_SECRET}" \
    -F "redirect_uri=${REDIRECT}" \
    -F "scope=${SCOPE}" \
    -F "code=${AUTH_CODE}")" || __die "POST /oauth/token failed"

ACCESS_TOKEN="$(echo "${token_resp}" | jq -r '.access_token')"
[ "${ACCESS_TOKEN}" != "null" ] || __die "no access_token in response: ${token_resp}"

# ---- step 5: verify ----
echo "Verifying token..."
me="$(curl -sf -H "Authorization: Bearer ${ACCESS_TOKEN}" \
    "${INSTANCE}/api/v1/accounts/verify_credentials" | jq -r '.acct')" \
    || __die "token verification failed"

echo "  authenticated as @${me}"

# ---- output ----
echo
echo "---"
echo "Add to your .env or shell rc:"
echo
echo "  export MASTODON_ACCESS_TOKEN=${ACCESS_TOKEN}"
echo
echo "Or write directly:"
echo "  echo \"MASTODON_ACCESS_TOKEN=${ACCESS_TOKEN}\" >> .env"
echo

printf "Write to .env now? [y/N]: "
read -r WRITE_ENV
if [[ "${WRITE_ENV}" =~ ^[Yy]$ ]]; then
    # Remove existing entry if present, then append.
    if [ -f .env ]; then
        grep -v "^MASTODON_ACCESS_TOKEN=" .env > .env.tmp && mv .env.tmp .env
    fi
    printf 'MASTODON_ACCESS_TOKEN=%s\n' "${ACCESS_TOKEN}" >> .env
    echo "Written to .env"
fi
