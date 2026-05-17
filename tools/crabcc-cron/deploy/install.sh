#!/usr/bin/env bash
# tools/crabcc-cron/deploy/install.sh — idempotent install onto Hetzner.
#
# Run as root on the target box (e.g. via ssh deploy@hetzner sudo bash).
# Symlinks /opt/crabcc-cron/ to a checkout under /srv/repos/crabcc/tools/crabcc-cron,
# installs /etc/cron.d/crabcc-cron, and creates /etc/crabcc-cron/env from
# env.example if it doesn't exist (operator must edit secrets in-place).

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-/srv/repos/crabcc}"
SRC="${REPO_ROOT}/tools/crabcc-cron"
DEST="/opt/crabcc-cron"
ETC="/etc/crabcc-cron"
CRON="/etc/cron.d/crabcc-cron"

if [[ "$EUID" -ne 0 ]]; then
  echo "must run as root" >&2
  exit 1
fi

# 1. Symlink /opt/crabcc-cron → repo checkout (so `git pull` updates the deploy).
if [[ -L "$DEST" || -e "$DEST" ]]; then
  rm -rf "$DEST"
fi
ln -s "$SRC" "$DEST"

# 2. /etc/crabcc-cron exists with env file. Don't overwrite if present.
mkdir -p "$ETC"
chmod 750 "$ETC"
chown root:deploy "$ETC"
if [[ ! -f "$ETC/env" ]]; then
  cp "$SRC/deploy/env.example" "$ETC/env"
  chmod 600 "$ETC/env"
  chown root:deploy "$ETC/env"
  echo "Created $ETC/env from env.example — edit secrets before running cron." >&2
fi

# 3. Install crontab.
cp "$SRC/deploy/crabcc-cron.cron" "$CRON"
chmod 644 "$CRON"
chown root:root "$CRON"

# 4. State + spool dirs.
mkdir -p /opt/crabcc-cron-state /opt/crabcc-cron-state/oss-fix
mkdir -p /srv/cron-agents/oss-fix
chown -R deploy:deploy /opt/crabcc-cron-state /srv/cron-agents

# 5. Smoke: shellcheck pass (catches typos before cron picks it up).
if command -v shellcheck >/dev/null 2>&1; then
  shellcheck -x "$SRC/bin/"* "$SRC/jobs/"* "$SRC/lib/"* || {
    echo "shellcheck failed; aborting" >&2
    exit 1
  }
fi

echo "crabcc-cron installed. Next cron tick will run."
