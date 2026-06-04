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
#   FLAVOR     OVH flavor   (default: c3-64 — 32 vCPU / 64 GB, ~$0.85/h)
#   IMAGE      base image   (default: "Ubuntu 24.04")
#   KEYPAIR    nova keypair name to inject (default: $USER-bench)
#   SSH_KEY    private key for SSH        (default: ~/.ssh/id_ed25519)
#   NETWORK    network to attach          (default: Ext-Net — public IP)
#   SWEEP_ARGS extra args to sweep.py     (default: --deep → 28-leg matrix that
#              saturates 32 vCPU; pin measurement to cores 0-3)
#
# Note OVH `c3` suffix is RAM-GiB at 1 vCPU : 2 GiB, so c3-64 = 32 vCPU / 64 GiB.
# The 28-leg --deep matrix is sized to keep all 32 vCPU busy through the
# single-threaded fat-LTO link phase (11 legs would leave the box half-idle).
set -euo pipefail

FLAVOR="${FLAVOR:-c3-64}"
IMAGE="${IMAGE:-Ubuntu 24.04}"
KEYPAIR="${KEYPAIR:-${USER}-bench}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519}"
NETWORK="${NETWORK:-Ext-Net}"
SWEEP_ARGS="${SWEEP_ARGS:---deep --pin 0-3}"
KEEP=0
SERVER_NAME="crabcc-bench-opt-bin-$(date +%s)"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

for arg in "$@"; do
  case "$arg" in
    --keep) KEEP=1 ;;
    --quick) SWEEP_ARGS="--quick" ;;   # replace, not append (drop --deep)
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
  binutils hyperfine git rsync python3 curl \
  linux-tools-common "linux-tools-$(uname -r)" linux-tools-generic >/dev/null 2>&1 || \
  sudo apt-get install -y -qq build-essential clang lld pkg-config libssl-dev \
  binutils hyperfine git rsync python3 curl >/dev/null
# Let perf read kernel/user stacks so the flamegraphs symbolize.
sudo sysctl -w kernel.perf_event_paranoid=-1 >/dev/null 2>&1 || true
sudo sysctl -w kernel.kptr_restrict=0 >/dev/null 2>&1 || true
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
command -v flamegraph >/dev/null || cargo install flamegraph --locked >/dev/null 2>&1 || true
# sccache: shares cached crate artifacts across the per-leg target dirs so the
# matrix doesn't recompile the whole dependency tree N times — turns redundant
# work into useful throughput on the rented box. Prebuilt binary (cargo install
# would burn minutes of the hour).
if ! command -v sccache >/dev/null; then
  SCV=v0.8.2
  curl -fsSL "https://github.com/mozilla/sccache/releases/download/${SCV}/sccache-${SCV}-x86_64-unknown-linux-musl.tar.gz" \
    | tar xz -C /tmp 2>/dev/null \
    && sudo install "/tmp/sccache-${SCV}-x86_64-unknown-linux-musl/sccache" /usr/local/bin/sccache 2>/dev/null || true
fi
echo "toolchain ready: $(rustc --version), bolt=$(command -v llvm-bolt || echo none), sccache=$(command -v sccache || echo none), perf=$(command -v perf || echo none), flamegraph=$(command -v flamegraph || echo none), nproc=$(nproc)"
BOOTSTRAP

echo ">> syncing repo → $IP"
rsync -az --delete -e "ssh -i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null" \
  --exclude '.git' --exclude 'target' --exclude 'bench' --exclude 'node_modules' \
  "$REPO_ROOT"/ "ubuntu@$IP:~/crabcc/"

# --flamegraph: symbolized SVGs for baseline+fastest.
echo ">> running sweep (this is the ~1h part)…"
$SSH "source ~/.cargo/env && cd ~/crabcc && \
  export SCCACHE_DIR=\$HOME/.cache/sccache SCCACHE_CACHE_SIZE=40G && \
  python3 scripts/bench-opt-bin/sweep.py $SWEEP_ARGS --flamegraph && \
  (command -v sccache >/dev/null && sccache --show-stats || true)"

echo ">> pulling ALL generated output back (reports, ndjson, logs, hyperfine, flamegraphs, tarballs)"
mkdir -p "$REPO_ROOT/bench/results"
RSYNC_SSH="ssh -i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null"
rsync -az -e "$RSYNC_SSH" "ubuntu@$IP:~/crabcc/bench/results/" "$REPO_ROOT/bench/results/"

# Durable publish: fan the newest run out to the configured sinks (_bench-results
# repo + LFS, Discord, Google Drive — see publish.sh). publish.sh runs HERE (on
# the machine that invoked provision-ovh.sh), so it uses this host's git creds
# and COMPOSIO_API_KEY / ~/.composio — not the throwaway VM's. Each sink is
# env-gated; unset ones are skipped. Set PUBLISH=0 to keep results local-only.
if [ "${PUBLISH:-1}" = "1" ]; then
  RUN_DIR="$(ls -dt "$REPO_ROOT"/bench/results/run-*/ 2>/dev/null | head -1)"
  if [ -n "$RUN_DIR" ]; then
    bash "$REPO_ROOT/scripts/bench-opt-bin/publish.sh" "$RUN_DIR" || \
      echo "   (publish had failures; bundle still in bench/results/)"
  else
    echo "   ! no run-*/ dir found to publish"
  fi
fi

echo ">> done. Report: bench/results/opt-bin-REPORT.md"
head -40 "$REPO_ROOT/bench/results/opt-bin-REPORT.md" 2>/dev/null || true
