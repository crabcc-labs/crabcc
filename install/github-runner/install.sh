#!/usr/bin/env bash
# Register a GitHub Actions self-hosted runner on Hetzner (or any Debian/Ubuntu host).
#
# Usage:
#   sudo bash install/github-runner/install.sh \
#     --url https://github.com/ORG/REPO \
#     --token RUNNER_REGISTRATION_TOKEN
#
# Optional:
#   --user deploy          # user that runs the service (default: current)
#   --labels self-hosted,linux,hetzner
#   --name hetzner-1
#   --gc-only              # only (re)install the disk-GC timer — no token,
#                          # no runner re-registration (fleet rollout path)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUNNER_USER="${SUDO_USER:-${USER:-root}}"
RUNNER_NAME="hetzner-$(hostname -s)"
RUNNER_LABELS="self-hosted,linux,hetzner"
REPO_URL=""
TOKEN=""
GC_ONLY=0
USER_EXPLICIT=0

usage() {
  sed -n '2,14p' "$0"
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --url) REPO_URL="$2"; shift 2 ;;
    --token) TOKEN="$2"; shift 2 ;;
    --user) RUNNER_USER="$2"; USER_EXPLICIT=1; shift 2 ;;
    --name) RUNNER_NAME="$2"; shift 2 ;;
    --labels) RUNNER_LABELS="$2"; shift 2 ;;
    --gc-only) GC_ONLY=1; shift ;;
    -h|--help) usage ;;
    *) echo "unknown arg: $1" >&2; usage ;;
  esac
done

if [ "$(id -u)" -ne 0 ]; then
  echo "run as root (sudo)" >&2
  exit 1
fi

RUNNER_UNIT=/etc/systemd/system/actions-runner.service

# In --gc-only mode, a `sudo ... --gc-only` invoked as root would default
# RUNNER_USER to root and target /root — missing the cache/_work of the
# account the runner actually runs as (e.g. a `deploy` user). Recover the
# real user + working dir from the existing runner service unit, unless the
# operator pinned --user explicitly.
if [ "$GC_ONLY" = 1 ] && [ "$USER_EXPLICIT" = 0 ] && [ -f "$RUNNER_UNIT" ]; then
  unit_user="$(awk -F= '/^User=/{print $2; exit}' "$RUNNER_UNIT")"
  unit_wd="$(awk -F= '/^WorkingDirectory=/{print $2; exit}' "$RUNNER_UNIT")"
  [ -n "$unit_user" ] && RUNNER_USER="$unit_user"
  [ -n "$unit_wd" ] && INSTALL_DIR_OVERRIDE="$unit_wd"
fi

RUNNER_HOME="$(eval echo "~${RUNNER_USER}")"
INSTALL_DIR="${INSTALL_DIR_OVERRIDE:-${RUNNER_HOME}/actions-runner}"

# ── Disk-GC timer ───────────────────────────────────────────────────────
# A single GitHub Actions cron only ever lands on one runner, so it can't
# keep the whole fleet clean. A host-local systemd timer makes every box
# self-prune Docker/cargo/apt/temp on a schedule, independent of GitHub.
# Idempotent — safe to re-run to refresh the units (this is exactly what
# --gc-only does for an already-provisioned fleet, no token required).
install_gc_timer() {
  echo "[runner] installing disk-GC timer..."
  install -m 0755 -o "$RUNNER_USER" -g "$RUNNER_USER" \
    "${SCRIPT_DIR}/runner-gc.sh" "${INSTALL_DIR}/runner-gc.sh"

  cat > /etc/systemd/system/actions-runner-gc.service <<EOF
[Unit]
Description=GitHub Actions runner disk GC (${RUNNER_NAME})

[Service]
Type=oneshot
User=${RUNNER_USER}
Environment=HOME=${RUNNER_HOME}
Environment=RUNNER_GC_WORK_TEMP=${INSTALL_DIR}/_work/_temp
ExecStart=/usr/bin/env bash ${INSTALL_DIR}/runner-gc.sh
EOF

  cat > /etc/systemd/system/actions-runner-gc.timer <<EOF
[Unit]
Description=Periodic disk GC for the GitHub Actions runner

[Timer]
# First run shortly after boot, then every 6h. Persistent catches up a
# missed window if the box was powered off.
OnBootSec=15min
OnUnitActiveSec=6h
Persistent=true

[Install]
WantedBy=timers.target
EOF

  systemctl daemon-reload
  systemctl enable --now actions-runner-gc.timer
  echo "[runner] disk-GC timer installed — next runs:"
  systemctl --no-pager list-timers actions-runner-gc.timer || true
}

# --gc-only: skip apt + runner download + registration; just (re)install
# the GC script + timer on an already-provisioned host.
if [ "$GC_ONLY" = 1 ]; then
  echo "[runner] --gc-only: installing disk-GC timer for ${RUNNER_NAME}"
  if [ ! -d "$INSTALL_DIR" ]; then
    mkdir -p "$INSTALL_DIR"
    chown "${RUNNER_USER}:${RUNNER_USER}" "$INSTALL_DIR"
  fi
  install_gc_timer
  exit 0
fi

[ -n "$REPO_URL" ] && [ -n "$TOKEN" ] || usage

echo "[runner] installing apt packages for crabcc CI..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y --no-install-recommends \
  ca-certificates curl git jq python3 python3-venv \
  build-essential pkg-config libssl-dev clang mold \
  sqlite3 zstd upx-ucl

mkdir -p "$INSTALL_DIR"
chown -R "${RUNNER_USER}:${RUNNER_USER}" "$INSTALL_DIR"

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) RUNNER_ARCH=x64 ;;
  aarch64) RUNNER_ARCH=arm64 ;;
  *) echo "unsupported arch: $ARCH" >&2; exit 1 ;;
esac

VER="$(curl -fsSL https://api.github.com/repos/actions/runner/releases/latest | jq -r .tag_name | sed 's/^v//')"
TARBALL="actions-runner-linux-${RUNNER_ARCH}-${VER}.tar.gz"
echo "[runner] downloading actions-runner ${VER} (${RUNNER_ARCH})..."
curl -fsSL -o "/tmp/${TARBALL}" "https://github.com/actions/runner/releases/download/v${VER}/${TARBALL}"

sudo -u "$RUNNER_USER" bash -c "
  set -euo pipefail
  cd '${INSTALL_DIR}'
  tar xzf '/tmp/${TARBALL}'
  ./config.sh --unattended \
    --url '${REPO_URL}' \
    --token '${TOKEN}' \
    --name '${RUNNER_NAME}' \
    --labels '${RUNNER_LABELS}' \
    --work '_work' \
    --replace
"

UNIT=/etc/systemd/system/actions-runner.service
cat > "$UNIT" <<EOF
[Unit]
Description=GitHub Actions runner (${RUNNER_NAME})
After=network.target

[Service]
Type=simple
User=${RUNNER_USER}
WorkingDirectory=${INSTALL_DIR}
ExecStart=${INSTALL_DIR}/run.sh
Restart=always
RestartSec=10
Environment=HOME=${RUNNER_HOME}

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now actions-runner
echo "[runner] installed — systemctl status actions-runner"
systemctl --no-pager status actions-runner || true

install_gc_timer
