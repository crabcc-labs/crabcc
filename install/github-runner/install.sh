#!/usr/bin/env bash
# Register a GitHub Actions self-hosted runner on Hetzner (or any Debian/Ubuntu host).
#
# Usage:
#   sudo bash install/github-runner/install.sh \
#     --url https://github.com/ORG/REPO \
#     --token RUNNER_REGISTRATION_TOKEN
#
# Optional:
#   --user deploy                   # user that runs the service (default: current)
#   --labels self-hosted,linux,hetzner
#   --name hetzner-1
#   --cache-volume /dev/sdb         # format + mount a dedicated block device for
#                                   # TMPDIR / CARGO_HOME / SCCACHE_DIR (20–50 GB).
#                                   # Fixes "curl: (23) Failure writing output" when
#                                   # the root filesystem fills up during toolchain
#                                   # downloads.  Safely skipped if the device is
#                                   # already ext4-formatted (idempotent).
#   --gc-only                       # only (re)install the disk-GC timer — no token,
#                                   # no runner re-registration (fleet rollout path)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUNNER_USER="${SUDO_USER:-${USER:-root}}"
RUNNER_NAME="hetzner-$(hostname -s)"
RUNNER_LABELS="self-hosted,linux,hetzner"
REPO_URL=""
TOKEN=""
GC_ONLY=0
USER_EXPLICIT=0
CACHE_VOLUME=""

# All runner-owned cache/temp data lives under this tree.
# When --cache-volume is supplied, this path becomes the mount point for
# the dedicated block device, giving tmp/cargo/sccache their own partition.
# Without a volume it's just a directory on the root fs — still co-locates
# everything under one roof for easy GC bookkeeping.
CACHE_BASE=/var/runner-data

usage() {
  sed -n '2,19p' "$0"
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --url) REPO_URL="$2"; shift 2 ;;
    --token) TOKEN="$2"; shift 2 ;;
    --user) RUNNER_USER="$2"; USER_EXPLICIT=1; shift 2 ;;
    --name) RUNNER_NAME="$2"; shift 2 ;;
    --labels) RUNNER_LABELS="$2"; shift 2 ;;
    --cache-volume) CACHE_VOLUME="$2"; shift 2 ;;
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

# ── Cache volume + directories ──────────────────────────────────────────
# Creates the CACHE_BASE tree and, when CACHE_VOLUME is set, formats and
# mounts the supplied block device there first.  Always idempotent.
setup_cache_dirs() {
  mkdir -p \
    "${CACHE_BASE}/tmp" \
    "${CACHE_BASE}/cargo" \
    "${CACHE_BASE}/sccache" \
    "${CACHE_BASE}/tool-cache"
  chown -R "${RUNNER_USER}:${RUNNER_USER}" "${CACHE_BASE}"
  # Sticky bit so multiple users can share /tmp semantics.
  chmod 1777 "${CACHE_BASE}/tmp"
  echo "[runner] cache dirs ready under ${CACHE_BASE}"
}

setup_cache_volume() {
  local dev="$1"
  echo "[runner] provisioning cache volume ${dev} → ${CACHE_BASE}"

  if [ ! -b "$dev" ]; then
    echo "ERROR: ${dev} is not a block device" >&2
    exit 1
  fi

  # Format only if the device carries no recognised filesystem yet.
  # Uses -F to allow formatting even if it looks mounted (e.g. after a
  # failed previous attempt); blkid is checked first as the safer guard.
  local fstype
  fstype="$(blkid -s TYPE -o value "$dev" 2>/dev/null || true)"
  if [ "$fstype" != "ext4" ]; then
    echo "[runner] formatting ${dev} as ext4 (label=runner-data)..."
    mkfs.ext4 -L runner-data -F "$dev"
  else
    echo "[runner] ${dev} already ext4 — skipping mkfs"
  fi

  mkdir -p "${CACHE_BASE}"

  # Persist mount across reboots.  The nofail option prevents the machine
  # from hanging at boot if the volume is temporarily detached.
  if ! grep -qs "${CACHE_BASE}" /etc/fstab; then
    echo "${dev} ${CACHE_BASE} ext4 defaults,nofail 0 2" >> /etc/fstab
    echo "[runner] added ${CACHE_BASE} to /etc/fstab"
  fi

  if ! mountpoint -q "${CACHE_BASE}"; then
    mount "${CACHE_BASE}"
    echo "[runner] mounted ${CACHE_BASE}"
  fi

  setup_cache_dirs
}

# ── Patch existing runner service unit ──────────────────────────────────
# Injects the four cache-redirect env vars into an already-installed unit
# when --gc-only --cache-volume is used on an existing runner.  Without
# this, the volume would be mounted but the runner service still exports
# the old TMPDIR/CARGO_HOME, leaving rustup/cargo writing to the root fs.
# Idempotent: skips the rewrite if the vars are already present.
patch_runner_unit() {
  local unit=/etc/systemd/system/actions-runner.service
  if [ ! -f "$unit" ]; then
    echo "[runner] no runner unit at ${unit} — skipping env patch (full install will write it)"
    return 0
  fi
  if grep -q "TMPDIR=${CACHE_BASE}/tmp" "$unit"; then
    echo "[runner] unit already has TMPDIR=${CACHE_BASE}/tmp — skipping patch"
    return 0
  fi
  echo "[runner] patching ${unit} with cache-volume env vars..."
  local tmp
  tmp=$(mktemp)
  # Insert the four Environment= lines immediately before [Install] so
  # they land inside [Service].  awk preserves the rest of the unit verbatim.
  awk -v base="${CACHE_BASE}" '
    /^\[Install\]/ {
      print "Environment=TMPDIR=" base "/tmp"
      print "Environment=CARGO_HOME=" base "/cargo"
      print "Environment=SCCACHE_DIR=" base "/sccache"
      print "Environment=RUNNER_TOOL_CACHE=" base "/tool-cache"
      print ""
    }
    { print }
  ' "$unit" > "$tmp"
  mv "$tmp" "$unit"
  systemctl daemon-reload
  echo "[runner] unit patched — run 'sudo systemctl restart actions-runner' to apply"
}

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
Environment=RUNNER_CACHE_BASE=${CACHE_BASE}
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
# the GC script + timer (and optionally the cache volume) on an already-
# provisioned host.  Pass --cache-volume /dev/sdb alongside --gc-only to
# add the data volume to an existing runner without re-registering it.
if [ "$GC_ONLY" = 1 ]; then
  echo "[runner] --gc-only: installing disk-GC timer for ${RUNNER_NAME}"
  if [ ! -d "$INSTALL_DIR" ]; then
    mkdir -p "$INSTALL_DIR"
    chown "${RUNNER_USER}:${RUNNER_USER}" "$INSTALL_DIR"
  fi
  if [ -n "$CACHE_VOLUME" ]; then
    setup_cache_volume "$CACHE_VOLUME"
  else
    setup_cache_dirs
  fi
  # Patch the existing runner unit so TMPDIR/CARGO_HOME/etc. take effect
  # after the operator runs `sudo systemctl restart actions-runner`.
  patch_runner_unit
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

# ── Cache volume ─────────────────────────────────────────────────────────
# Must happen before the runner download so the tarball can land in the
# dedicated tmp dir rather than the root filesystem's /tmp.
if [ -n "$CACHE_VOLUME" ]; then
  setup_cache_volume "$CACHE_VOLUME"
else
  setup_cache_dirs
fi

RUNNER_HOME="$(eval echo "~${RUNNER_USER}")"
INSTALL_DIR="${INSTALL_DIR_OVERRIDE:-${RUNNER_HOME}/actions-runner}"
mkdir -p "$INSTALL_DIR"
chown -R "${RUNNER_USER}:${RUNNER_USER}" "$INSTALL_DIR"

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) RUNNER_ARCH=x64 ;;
  aarch64) RUNNER_ARCH=arm64 ;;
  *) echo "unsupported arch: $ARCH" >&2; exit 1 ;;
esac

# Download to the dedicated tmp dir so the write never fills the root fs.
DOWNLOAD_TMP="${CACHE_BASE}/tmp"
VER="$(curl -fsSL https://api.github.com/repos/actions/runner/releases/latest | jq -r .tag_name | sed 's/^v//')"
TARBALL="actions-runner-linux-${RUNNER_ARCH}-${VER}.tar.gz"
echo "[runner] downloading actions-runner ${VER} (${RUNNER_ARCH}) → ${DOWNLOAD_TMP}..."
curl -fsSL -o "${DOWNLOAD_TMP}/${TARBALL}" \
  "https://github.com/actions/runner/releases/download/v${VER}/${TARBALL}"

sudo -u "$RUNNER_USER" bash -c "
  set -euo pipefail
  cd '${INSTALL_DIR}'
  tar xzf '${DOWNLOAD_TMP}/${TARBALL}'
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
# Redirect all temp/cache I/O off the root filesystem.
# TMPDIR is inherited by every child process, including dtolnay/rust-toolchain
# (rustup download) and cargo build steps.  CARGO_HOME and SCCACHE_DIR keep
# registry blobs + sccache entries on the data volume rather than home.
Environment=TMPDIR=${CACHE_BASE}/tmp
Environment=CARGO_HOME=${CACHE_BASE}/cargo
Environment=SCCACHE_DIR=${CACHE_BASE}/sccache
Environment=RUNNER_TOOL_CACHE=${CACHE_BASE}/tool-cache

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now actions-runner
echo "[runner] installed — systemctl status actions-runner"
systemctl --no-pager status actions-runner || true

install_gc_timer
