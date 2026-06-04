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
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUNNER_USER="${SUDO_USER:-${USER:-root}}"
RUNNER_NAME="hetzner-$(hostname -s)"
RUNNER_LABELS="self-hosted,linux,hetzner"
REPO_URL=""
TOKEN=""

usage() {
  sed -n '2,12p' "$0"
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --url) REPO_URL="$2"; shift 2 ;;
    --token) TOKEN="$2"; shift 2 ;;
    --user) RUNNER_USER="$2"; shift 2 ;;
    --name) RUNNER_NAME="$2"; shift 2 ;;
    --labels) RUNNER_LABELS="$2"; shift 2 ;;
    -h|--help) usage ;;
    *) echo "unknown arg: $1" >&2; usage ;;
  esac
done

[ -n "$REPO_URL" ] && [ -n "$TOKEN" ] || usage

if [ "$(id -u)" -ne 0 ]; then
  echo "run as root (sudo)" >&2
  exit 1
fi

echo "[runner] installing apt packages for crabcc CI..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y --no-install-recommends \
  ca-certificates curl git jq python3 python3-venv \
  build-essential pkg-config libssl-dev clang mold \
  sqlite3 zstd upx-ucl

RUNNER_HOME="$(eval echo "~${RUNNER_USER}")"
INSTALL_DIR="${RUNNER_HOME}/actions-runner"
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

# ── Disk-GC timer ───────────────────────────────────────────────────────
# A single GitHub Actions cron only ever lands on one runner, so it can't
# keep the whole fleet clean. Install a host-local systemd timer instead:
# every box self-prunes Docker/cargo/apt/temp on a schedule, independent of
# GitHub. Idempotent — re-running install.sh refreshes the units.
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
