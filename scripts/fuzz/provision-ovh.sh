#!/usr/bin/env bash
# Provision a FRESH OVHcloud Public Cloud instance (Ubuntu 22.04/24.04) to run
# the crabcc fuzzing matrix, then launch it. Run as a sudo-capable user.
#
# Two ways to get the source onto the box:
#   A) rsync your checkout up first, then run this with CRABCC_DIR pointing at it
#        rsync -az --exclude target --exclude .git ./ ubuntu@<ip>:~/crabcc/
#        ssh ubuntu@<ip> 'CRABCC_DIR=~/crabcc ~/crabcc/scripts/fuzz/provision-ovh.sh'
#   B) clone (needs a deploy key on the box for the private repo)
#        ssh ubuntu@<ip> 'REPO_URL=git@github.com:crabcc-labs/crabcc.git bash -s' < scripts/fuzz/provision-ovh.sh
#
# Env knobs: CRABCC_DIR (default ~/crabcc), REPO_URL (clone source), DURATION (sec/target, default 2700).
set -euo pipefail

CRABCC_DIR="${CRABCC_DIR:-$HOME/crabcc}"
REPO_URL="${REPO_URL:-}"
DURATION="${DURATION:-2700}"

echo "[provision] apt build deps ..."
sudo apt-get update -y
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y \
  build-essential clang lld llvm pkg-config libssl-dev cmake git curl ca-certificates

if ! command -v rustup >/dev/null 2>&1; then
  echo "[provision] installing rustup ..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
rustup toolchain install nightly --profile minimal
command -v cargo-fuzz >/dev/null 2>&1 || cargo install cargo-fuzz

if [ ! -d "$CRABCC_DIR" ]; then
  [ -n "$REPO_URL" ] || { echo "set REPO_URL=... or rsync the repo to $CRABCC_DIR" >&2; exit 2; }
  echo "[provision] cloning $REPO_URL -> $CRABCC_DIR"
  git clone "$REPO_URL" "$CRABCC_DIR"
fi

echo "[provision] handing off to matrix.sh (${DURATION}s/target) ..."
exec "$CRABCC_DIR/scripts/fuzz/matrix.sh" "$DURATION" "$CRABCC_DIR"
