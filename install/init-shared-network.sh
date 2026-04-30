#!/usr/bin/env sh
# crabcc shared Docker network bootstrap — issues #105 / #109.
#
# Creates the `crabcc-shared` bridge network if missing. Both
# install/ollama-stack/docker-compose.yml and install/dev/docker-compose.yml
# attach services to this network for cross-stack DNS resolution
# (e.g., the dev `crabcc` container reaches `litellm:4000` without going
# through the host port).
#
# Idempotent: re-running is a no-op when the network already exists.
#
# Usage:
#   install/init-shared-network.sh         # create if missing
#   install/init-shared-network.sh --info  # print current network state
#   install/init-shared-network.sh --rm    # delete (only when no services attached)

set -eu

NETWORK="crabcc-shared"

case "${1:-}" in
  --info)
    if docker network inspect "$NETWORK" >/dev/null 2>&1; then
      docker network inspect "$NETWORK" \
        --format 'name={{.Name}} driver={{.Driver}} scope={{.Scope}} containers={{len .Containers}}'
    else
      printf 'network %s does not exist (create it: %s)\n' "$NETWORK" "$0"
      exit 1
    fi
    exit 0
    ;;
  --rm)
    if docker network inspect "$NETWORK" >/dev/null 2>&1; then
      docker network rm "$NETWORK"
    else
      printf 'network %s does not exist; nothing to remove\n' "$NETWORK"
    fi
    exit 0
    ;;
  -h|--help)
    sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
esac

if docker network inspect "$NETWORK" >/dev/null 2>&1; then
  printf 'network %s already exists\n' "$NETWORK"
  exit 0
fi

if ! command -v docker >/dev/null 2>&1; then
  printf 'docker not in PATH; install from https://docs.docker.com/get-docker/\n' >&2
  exit 1
fi

docker network create \
  --driver bridge \
  --label com.crabcc.role=shared \
  --label com.crabcc.created-by="$(basename "$0")" \
  "$NETWORK"

printf 'created network %s\n' "$NETWORK"
