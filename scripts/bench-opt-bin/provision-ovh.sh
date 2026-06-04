#!/usr/bin/env bash
# provision-ovh.sh — spin a throwaway OVHcloud Public Cloud instance, run the
# bench-opt-bin sweep on it, pull the results back, and tear it down.
#
# OVHcloud Public Cloud is OpenStack under the hood, so this drives the
# `openstack` CLI. Authenticate by sourcing your project's openrc file first
# (Horizon → Project → API → "OpenStack RC file v3"):
#
#   source ~/ovh-openrc.sh        # sets OS_AUTH_URL, OS_PROJECT_ID, creds…
#   ./scripts/bench-opt-bin/provision-ovh.sh
#
# A `trap` deletes the instance on ANY exit path (success, failure, Ctrl-C) so
# a crashed run never leaves a billable VM running. Pass --keep to override.
#
# Knobs (env):
#   FLAVOR     OVH flavor   (default: c3-32 — 32 vCPU / 64 GB, ~1h sweep box)
#   IMAGE      base image   (default: "Ubuntu 24.04")
#   KEYPAIR    nova keypair name to inject (default: $USER-bench)
#   SSH_KEY    private key for SSH        (default: ~/.ssh/id_ed25519)
#   NETWORK    network to attach          (default: Ext-Net — public IP)
#   SWEEP_ARGS extra args to sweep.py     (default: empty → full matrix)
set -euo pipefail

FLAVOR="${FLAVOR:-c3-32}"
IMAGE="${IMAGE:-Ubuntu 24.04}"
KEYPAIR="${KEYPAIR:-${USER}-bench}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519}"
NETWORK="${NETWORK:-Ext-Net}"
SWEEP_ARGS="${SWEEP_ARGS:-}"
KEEP=0
SERVER_NAME="crabcc-bench-opt-bin-$(date +%s)"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

for arg in "$@"; do
  case "$arg" in
    --keep) KEEP=1 ;;
    --quick) SWEEP_ARGS="$SWEEP_ARGS --quick" ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

command -v openstack >/dev/null || { echo "openstack CLI not found (pip install python-openstackclient)"; exit 1; }
[ -n "${OS_AUTH_URL:-}" ] || { echo "source your OVH openrc first (OS_AUTH_URL unset)"; exit 1; }

SERVER_ID=""
cleanup() {
  if [ "$KEEP" = "1" ]; then
    echo ">> --keep set; leaving $SERVER_NAME ($SERVER_ID) RUNNING — remember to delete it."
    return
  fi
  if [ -n "$SERVER_ID" ]; then
    echo ">> tearing down $SERVER_NAME ($SERVER_ID)…"
    openstack server delete "$SERVER_ID" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

echo ">> ensuring keypair '$KEYPAIR' exists"
if ! openstack keypair show "$KEYPAIR" >/dev/null 2>&1; then
  openstack keypair create --public-key "${SSH_KEY}.pub" "$KEYPAIR" >/dev/null
fi

echo ">> creating $SERVER_NAME (flavor=$FLAVOR image='$IMAGE')"
SERVER_ID=$(openstack server create \
  --flavor "$FLAVOR" --image "$IMAGE" --key-name "$KEYPAIR" \
  --network "$NETWORK" --wait -f value -c id "$SERVER_NAME")
echo "   id=$SERVER_ID"

IP=$(openstack server show "$SERVER_ID" -f json \
  | python3 -c 'import sys,json;a=json.load(sys.stdin)["addresses"];print([v[0] for v in (a.values() if isinstance(a,dict) else [])][0] if isinstance(a,dict) else a.split("=")[-1].strip())' 2>/dev/null \
  || openstack server show "$SERVER_ID" -f value -c addresses | sed 's/.*=//')
echo "   ip=$IP"

SSH="ssh -i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 ubuntu@$IP"
echo ">> waiting for SSH…"
for i in $(seq 1 40); do
  if $SSH true 2>/dev/null; then break; fi
  sleep 6
  [ "$i" = 40 ] && { echo "SSH never came up"; exit 1; }
done

echo ">> bootstrapping toolchain (rust + llvm-bolt + hyperfine + binutils)…"
$SSH 'bash -s' <<'BOOTSTRAP'
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
sudo apt-get update -qq
sudo apt-get install -y -qq build-essential clang lld pkg-config libssl-dev \
  binutils hyperfine git rsync python3 curl >/dev/null
# BOLT: try the distro 'bolt' package, fall back to apt.llvm.org.
if ! command -v llvm-bolt >/dev/null; then
  sudo apt-get install -y -qq bolt >/dev/null 2>&1 || {
    curl -fsSL https://apt.llvm.org/llvm.sh | sudo bash -s -- 18 >/dev/null 2>&1 || true
    sudo apt-get install -y -qq bolt-18 >/dev/null 2>&1 || true
  }
fi
# Symlink versioned BOLT tools onto PATH if needed.
for t in llvm-bolt merge-fdata; do
  command -v $t >/dev/null || {
    p=$(ls /usr/lib/llvm-*/bin/$t /usr/bin/${t}-* 2>/dev/null | head -1 || true)
    [ -n "$p" ] && sudo ln -sf "$p" /usr/local/bin/$t || true
  }
done
if ! command -v cargo >/dev/null; then
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
fi
source "$HOME/.cargo/env"
rustup component add llvm-tools-preview >/dev/null 2>&1 || true
echo "toolchain ready: $(rustc --version), bolt=$(command -v llvm-bolt || echo none)"
BOOTSTRAP

echo ">> syncing repo → $IP"
rsync -az --delete -e "ssh -i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null" \
  --exclude '.git' --exclude 'target' --exclude 'bench' --exclude 'node_modules' \
  "$REPO_ROOT"/ "ubuntu@$IP:~/crabcc/"

echo ">> running sweep (this is the ~1h part)…"
$SSH "source ~/.cargo/env && cd ~/crabcc && python3 scripts/bench-opt-bin/sweep.py $SWEEP_ARGS"

echo ">> pulling results back"
mkdir -p "$REPO_ROOT/bench/results"
rsync -az -e "ssh -i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null" \
  "ubuntu@$IP:~/crabcc/bench/results/" "$REPO_ROOT/bench/results/"

echo ">> done. Report: bench/results/opt-bin-REPORT.md"
cat "$REPO_ROOT/bench/results/opt-bin-REPORT.md" 2>/dev/null | head -40 || true
