#!/usr/bin/env bash
# Report Docker Hub usage for the account's images. Markdown to stdout.
#
# Env: DOCKER_USER, DOCKER_PAT (CI passes secrets.docker_user / docker_pat).
#      DOCKER_NAMESPACE (optional; defaults to DOCKER_USER).
#
# Reports, per repo in the namespace: pull_count, private?, last_updated.
# (Docker Build Cloud minutes live in a separate Build Cloud API/dashboard and
# are not covered by a registry PAT — see the note printed at the end.)
set -euo pipefail
: "${DOCKER_USER:?set DOCKER_USER}"
: "${DOCKER_PAT:?set DOCKER_PAT}"
NS="${DOCKER_NAMESPACE:-$DOCKER_USER}"

# Build the JSON with jq so special chars in the PAT (", \, etc.) are escaped.
tok="$(jq -n --arg u "$DOCKER_USER" --arg p "$DOCKER_PAT" '{username:$u,password:$p}' \
  | curl -fsS -H 'Content-Type: application/json' -d @- \
    https://hub.docker.com/v2/users/login/ | jq -r '.token // empty')"
[ -n "$tok" ] || { echo "Docker Hub login failed (check docker_user/docker_pat)"; exit 1; }

echo "## Docker Hub usage — namespace \`${NS}\` ($(date -u +%Y-%m-%dT%H:%MZ))"
echo
echo "| repo | pulls | private | last updated |"
echo "|---|---:|:---:|---|"
curl -fsS -H "Authorization: JWT ${tok}" \
  "https://hub.docker.com/v2/repositories/${NS}/?page_size=100" \
  | jq -r '.results[]? | "| \(.name) | \(.pull_count) | \(if .is_private then "yes" else "no" end) | \(.last_updated // "-") |"' \
  | sort
echo
echo "_Build Cloud minutes are not exposed via the registry PAT — see the Docker"
echo "Build Cloud dashboard (Settings -> Builds) for builder minutes/usage._"
