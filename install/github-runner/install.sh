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
#                                   # TMPDIR / CARGO_HOME / SCCACHE_DIR (100 GB
#                                   # standard on runner-01 / runner-01b).
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
RUNNER_LABELS="self-hosted,linux,hetzner,gh-runner"
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

# Locate all runner service units on this host. Our own install.sh writes
# one unit at RUNNER_UNIT; GitHub's own svc.sh uses the naming convention
# actions.runner.<owner>-<repo>.<runner-name>.service (one per runner
# process, e.g. runner-01 and runner-01b on the same machine).
find_all_runner_units() {
  [ -f "$RUNNER_UNIT" ] && echo "$RUNNER_UNIT"
  find /etc/systemd/system -maxdepth 1 -name 'actions.runner.*.service' 2>/dev/null
}

find_runner_unit() {
  find_all_runner_units | head -1
}

# In --gc-only mode, a `sudo ... --gc-only` invoked as root would default
# RUNNER_USER to root and target /root — missing the cache/_work of the
# account the runner actually runs as (e.g. a `deploy` user). Recover the
# real user + working dir from the existing runner service unit, unless the
# operator pinned --user explicitly.
if [ "$GC_ONLY" = 1 ] && [ "$USER_EXPLICIT" = 0 ]; then
  _actual_unit="$(find_runner_unit)"
  if [ -n "$_actual_unit" ]; then
    unit_user="$(awk -F= '/^User=/{print $2; exit}' "$_actual_unit")"
    unit_wd="$(awk -F= '/^WorkingDirectory=/{print $2; exit}' "$_actual_unit")"
    [ -n "$unit_user" ] && RUNNER_USER="$unit_user"
    [ -n "$unit_wd" ] && INSTALL_DIR_OVERRIDE="$unit_wd"
  fi
  unset _actual_unit
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

  # Format only when the device carries no recognised filesystem.
  # If blkid finds a non-ext4 type (XFS, Btrfs, existing data, …), refuse
  # rather than silently destroying contents with mkfs.ext4 -F.
  local fstype
  fstype="$(blkid -s TYPE -o value "$dev" 2>/dev/null || true)"
  if [ -z "$fstype" ]; then
    echo "[runner] no filesystem on ${dev} — formatting as ext4 (label=runner-data)..."
    mkfs.ext4 -L runner-data -F "$dev"
  elif [ "$fstype" = "ext4" ]; then
    echo "[runner] ${dev} already ext4 — skipping mkfs"
  else
    echo "ERROR: ${dev} contains a ${fstype} filesystem — refusing to reformat." >&2
    echo "ERROR: Detach or wipe the volume manually, then retry --cache-volume." >&2
    exit 1
  fi

  mkdir -p "${CACHE_BASE}"

  # If the Hetzner automount agent already mounted the device somewhere else
  # (typically /mnt/HC_Volume_*), unmount it before the fstab-controlled
  # mount at CACHE_BASE.  Without this, `mount CACHE_BASE` would fail with
  # "device is already mounted" even though CACHE_BASE is not yet a mountpoint.
  local current_mount
  current_mount="$(findmnt -n -o TARGET --source "$dev" 2>/dev/null || true)"
  if [ -n "$current_mount" ] && [ "$current_mount" != "$CACHE_BASE" ]; then
    echo "[runner] unmounting automount at ${current_mount}..."
    umount "$dev"
  fi

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
  local units patched=0
  units="$(find_all_runner_units)"
  if [ -z "$units" ]; then
    echo "[runner] no runner unit found — skipping env patch (full install will write it)"
    return 0
  fi

  while IFS= read -r unit; do
    [ -z "$unit" ] && continue
    if grep -q "TMPDIR=${CACHE_BASE}/tmp" "$unit" \
       && grep -q "SCCACHE_CACHE_SIZE=" "$unit" \
       && grep -q "CARGO_TARGET_DIR=" "$unit" \
       && grep -q "LimitNOFILE=" "$unit"; then
      echo "[runner] unit already patched: ${unit} — skipping"
      continue
    fi
    echo "[runner] patching ${unit} with cache-volume env vars..."
    # Per-runner target dir: two runner processes on one host (runner-01 /
    # runner-01b) must NOT share CARGO_TARGET_DIR, or cargo's exclusive target
    # lock serializes their builds. Derive a unique tag from the unit name
    # (e.g. actions.runner.<owner>-<repo>.runner-01.service → "runner-01").
    local tag
    tag="$(basename "$unit" .service)"; tag="${tag##*.}"
    # Write to the data volume to avoid filling the (often near-full) root fs.
    local tmp="${CACHE_BASE}/tmp/runner-unit-patch-$$.tmp"
    # Insert the Environment= lines immediately before [Install] so they land
    # inside [Service]. awk preserves the rest of the unit verbatim.
    awk -v base="${CACHE_BASE}" -v tag="${tag}" '
      # Strip any pre-existing cache-env + limit lines so re-runs are idempotent
      # even when an older patch omitted some vars (e.g. CARGO_TARGET_DIR).
      /^Environment=(TMPDIR|CARGO_HOME|CARGO_TARGET_DIR|SCCACHE_DIR|SCCACHE_CACHE_SIZE|RUNNER_TOOL_CACHE)=/ { next }
      /^(LimitNOFILE|LimitNPROC)=/ { next }
      /^\[Install\]/ {
        print "Environment=TMPDIR=" base "/tmp"
        print "Environment=CARGO_HOME=" base "/cargo"
        print "Environment=SCCACHE_DIR=" base "/sccache"
        print "Environment=SCCACHE_CACHE_SIZE=15G"
        print "Environment=RUNNER_TOOL_CACHE=" base "/tool-cache"
        print "Environment=CARGO_TARGET_DIR=" base "/target/" tag
        print "LimitNOFILE=1048576"
        print "LimitNPROC=unlimited"
        print ""
      }
      { print }
    ' "$unit" > "$tmp"
    mv "$tmp" "$unit"
    patched=$((patched + 1))
  done <<< "$units"

  if [ "$patched" -gt 0 ]; then
    systemctl daemon-reload
    echo "[runner] patched ${patched} unit(s) — runners will pick up new env vars on next restart"
  fi
}

# ── Docker daemon config: log rotation + default shm size ────────────────
# Log limits: without them, containers that emit continuous output fill
# /var/lib/docker/containers/*/...log on the root fs unboundedly. Cap each at
# 10 MB × 3 files (~30 MB worst-case per container).
# default-shm-size: containers default to a tiny 64 MB /dev/shm, which is too
# small for the testcontainers e2e suite (redis, etc.); bump to 2 GB.
# Idempotent: re-applies if either setting is missing.
configure_docker() {
  if ! command -v docker >/dev/null 2>&1; then
    echo "[runner] docker not found — skipping daemon config"
    return 0
  fi
  local cfg=/etc/docker/daemon.json
  if [ -f "$cfg" ] && grep -q '"max-size"' "$cfg" && grep -q '"default-shm-size"' "$cfg"; then
    echo "[runner] docker daemon already configured (logging + shm)"
    return 0
  fi
  if [ -f "$cfg" ]; then
    # Merge into existing daemon.json (requires jq, which we installed above).
    jq '. + {"log-driver":"json-file","log-opts":{"max-size":"10m","max-file":"3"},"default-shm-size":"2G"}' \
      "$cfg" > "${cfg}.tmp" && mv "${cfg}.tmp" "$cfg"
  else
    mkdir -p /etc/docker
    cat > "$cfg" <<'DOCKEREOF'
{
  "log-driver": "json-file",
  "log-opts": {
    "max-size": "10m",
    "max-file": "3"
  },
  "default-shm-size": "2G"
}
DOCKEREOF
  fi
  echo "[runner] docker daemon configured (10 MB × 3 log files, 2 GB default shm)"
  # HUP reloads the config without restarting existing containers.
  systemctl reload docker 2>/dev/null || true
}

# ── Host /dev/shm sizing ─────────────────────────────────────────────────
# Default /dev/shm is 50% of RAM; on the 8 GB runners that's ~4 GB, and heavy
# parallel test/link steps (plus anything using POSIX shm) can exhaust it.
# Pin it to 6 GB via fstab and remount live. tmpfs is swap-backed, so the size
# is a ceiling, not a reservation — safe to set above the typical default.
SHM_SIZE="${SHM_SIZE:-6g}"
configure_shm() {
  if grep -qE '^[^[:space:]]+[[:space:]]+/dev/shm[[:space:]]' /etc/fstab; then
    # Update an existing entry's size= option in place.
    sed -i -E "s|^([^[:space:]]+[[:space:]]+/dev/shm[[:space:]]+tmpfs[[:space:]]+)[^[:space:]]+|\1defaults,nosuid,nodev,size=${SHM_SIZE}|" /etc/fstab
  else
    echo "tmpfs /dev/shm tmpfs defaults,nosuid,nodev,size=${SHM_SIZE} 0 0" >> /etc/fstab
  fi
  # Remount live so the change takes effect without a reboot.
  mount -o "remount,size=${SHM_SIZE}" /dev/shm 2>/dev/null \
    || echo "[runner] WARN: live /dev/shm remount failed — applies on next boot"
  echo "[runner] /dev/shm sized to ${SHM_SIZE} ($(df -h /dev/shm | awk 'NR==2{print $2}'))"
}

# ── Disk-watchdog timer (15 min, threshold-guarded) ──────────────────────
# The 4h full-GC timer is too slow to catch a disk spike during a heavy
# build. This watchdog fires every 15 minutes but exits immediately when
# root fs is below 75% (runner-gc.sh --if-above), so it's near-zero-cost
# on a healthy host and aggressive when the disk is actually in trouble.
install_disk_watch_timer() {
  echo "[runner] installing disk-watchdog timer..."

  cat > /etc/systemd/system/actions-runner-disk-watch.service <<EOF
[Unit]
Description=GitHub Actions runner disk watchdog (${RUNNER_NAME})
After=actions-runner-gc.service

[Service]
Type=oneshot
User=${RUNNER_USER}
Environment=HOME=${RUNNER_HOME}
Environment=RUNNER_CACHE_BASE=${CACHE_BASE}
Environment=CARGO_HOME=${CACHE_BASE}/cargo
Environment=SCCACHE_DIR=${CACHE_BASE}/sccache
Environment=RUNNER_TOOL_CACHE=${CACHE_BASE}/tool-cache
Environment=RUNNER_TARGET_BASE=${CACHE_BASE}/target
# Only prune when root fs >= 75%; exits immediately otherwise.
ExecStart=/usr/bin/env bash ${INSTALL_DIR}/runner-gc.sh --if-above 75
EOF

  cat > /etc/systemd/system/actions-runner-disk-watch.timer <<EOF
[Unit]
Description=15-min disk watchdog for the GitHub Actions runner

[Timer]
OnBootSec=2min
OnUnitActiveSec=15min
Persistent=false

[Install]
WantedBy=timers.target
EOF

  systemctl daemon-reload
  systemctl enable --now actions-runner-disk-watch.timer
  echo "[runner] disk-watchdog timer installed (15 min, threshold 75%)"
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
Environment=CARGO_HOME=${CACHE_BASE}/cargo
Environment=SCCACHE_DIR=${CACHE_BASE}/sccache
Environment=RUNNER_TOOL_CACHE=${CACHE_BASE}/tool-cache
Environment=RUNNER_TARGET_BASE=${CACHE_BASE}/target
ExecStart=/usr/bin/env bash ${INSTALL_DIR}/runner-gc.sh
EOF

  cat > /etc/systemd/system/actions-runner-gc.timer <<EOF
[Unit]
Description=Periodic disk GC for the GitHub Actions runner

[Timer]
# First run 5 min after boot (catches leftover temp from prior session), then
# every 4h. Persistent catches up a missed window after downtime.
OnBootSec=5min
OnUnitActiveSec=4h
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
  configure_docker
  configure_shm
  install_gc_timer
  install_disk_watch_timer
  # Pre-bake the CI build tools so jobs never run `apt-get install` at the
  # setup step (which has ENOSPC'd). Reclaim first via the freshly-installed GC
  # script so apt has room to write its cache, then ensure mold + ripgrep. Once
  # these are present the `command -v mold || apt-get install` guard in ci.yml
  # is a permanent no-op. Best-effort: a still-full host logs a clear warning.
  if ! command -v mold >/dev/null 2>&1 || ! command -v rg >/dev/null 2>&1; then
    echo "[runner] --gc-only: baking CI build tools (mold, ripgrep)"
    bash "$(dirname "${BASH_SOURCE[0]}")/runner-gc.sh" --deep 2>/dev/null || true
    apt-get update -qq 2>/dev/null || true
    apt-get install -y --no-install-recommends mold ripgrep 2>/dev/null \
      || echo "[runner] WARN: mold/ripgrep install failed (apt ENOSPC?) — inspect 'df -h; df -i' on this host; CI will keep apt-get'ing per job until it succeeds"
  fi
  exit 0
fi

[ -n "$REPO_URL" ] && [ -n "$TOKEN" ] || usage

echo "[runner] installing apt packages for crabcc CI..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
# Preinstall everything the CI jobs install at runtime so their per-job
# `command -v X || apt-get install X` guards are all no-ops — no per-job apt
# writes to the root fs (mold, ripgrep) and no ENOSPC at the setup step.
# mold: ci.yml linker. ripgrep: shell_rewrite_e2e::find_swaps_to_ripgrep.
# sqlite3 + zstd: index-publish.yml. Keep this in sync with the workflows.
apt-get install -y --no-install-recommends \
  ca-certificates curl git jq python3 python3-venv \
  build-essential pkg-config libssl-dev clang mold \
  sqlite3 zstd upx-ucl ripgrep

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
# Raise the fd + process ceilings: big parallel rustc/link jobs plus sccache
# (many concurrent open objects) blow past the systemd default of 1024 open
# files. Inherited by every job step.
LimitNOFILE=1048576
LimitNPROC=unlimited
Environment=HOME=${RUNNER_HOME}
# Redirect all temp/cache I/O off the root filesystem.
# TMPDIR is inherited by every child process, including dtolnay/rust-toolchain
# (rustup download) and cargo build steps.  CARGO_HOME and SCCACHE_DIR keep
# registry blobs + sccache entries on the data volume rather than home.
# CARGO_TARGET_DIR is the big one: without it, every build writes a multi-GB
# target/ tree into _work on the root fs. Tagged by runner name. On a host
# running two runner processes the svc.sh units get distinct tags via
# patch_runner_unit, so their builds never share cargo's exclusive target lock;
# this self-written single unit is per host (one runner).
Environment=TMPDIR=${CACHE_BASE}/tmp
Environment=CARGO_HOME=${CACHE_BASE}/cargo
Environment=SCCACHE_DIR=${CACHE_BASE}/sccache
Environment=SCCACHE_CACHE_SIZE=15G
Environment=RUNNER_TOOL_CACHE=${CACHE_BASE}/tool-cache
Environment=CARGO_TARGET_DIR=${CACHE_BASE}/target/${RUNNER_NAME}

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now actions-runner
echo "[runner] installed — systemctl status actions-runner"
systemctl --no-pager status actions-runner || true

configure_docker
configure_shm
install_gc_timer
install_disk_watch_timer
