#!/usr/bin/env bash
# Provision an ephemeral OVHCloud instance for benchmarking.
#
# Creates an OVHCloud Public Cloud instance, embeds a GitHub runner
# registration token in the cloud-init script, and waits for the ephemeral
# runner to come online before returning.  Outputs instance_id and
# runner_label to $GITHUB_OUTPUT for the bench + teardown jobs.
#
# Called by .github/workflows/bench.yml — not intended for direct use.
#
# Required environment variables:
#   OS_AUTH_URL, OS_TENANT_ID, OS_USERNAME, OS_PASSWORD, OS_REGION_NAME,
#   OS_USER_DOMAIN_NAME, OS_PROJECT_DOMAIN_NAME
#   RUNNER_PAT    GitHub PAT with manage_runners:repo scope
#   GH_REPO       e.g. crabcc-labs/crabcc
#   RUN_ID        ${{ github.run_id }}
#   FLAVOR        OVHCloud flavor e.g. c2-15

set -euo pipefail
log() { echo "[bench-provision] $*"; }

RUNNER_NAME="bench-ovh-${RUN_ID}"
# Each run gets a unique label so concurrent dispatches (while discouraged
# by the concurrency group) can't accidentally cross-assign runners.
RUNNER_LABEL="ovhcloud-bench-${RUN_ID}"

# ── Install OpenStack CLI ─────────────────────────────────────────────────
if ! command -v openstack >/dev/null 2>&1; then
  log "installing python3-openstackclient..."
  sudo apt-get install -y --no-install-recommends python3-openstackclient -qq
fi

# ── GitHub runner registration token ─────────────────────────────────────
log "fetching runner registration token..."
REG_RESP=$(curl -fsSL -X POST \
  -H "Authorization: Bearer ${RUNNER_PAT}" \
  -H "Accept: application/vnd.github+json" \
  -H "X-GitHub-Api-Version: 2022-11-28" \
  "https://api.github.com/repos/${GH_REPO}/actions/runners/registration-token")
REG_TOKEN=$(echo "$REG_RESP" | jq -r .token)
if [ -z "${REG_TOKEN:-}" ] || [ "$REG_TOKEN" = "null" ]; then
  echo "ERROR: failed to get registration token — check RUNNER_REGISTRATION_PAT has manage_runners:repo scope" >&2
  echo "API response: ${REG_RESP}" >&2
  exit 1
fi
log "registration token obtained (valid 1h)"

# ── Cloud-init script ─────────────────────────────────────────────────────
# The instance installs build dependencies, downloads the runner, configures
# it as ephemeral (single-job auto-deregister), then starts it in the
# background so cloud-init finishes quickly and the runner picks up the
# bench job as soon as the instance is live.
REPO_URL="https://github.com/${GH_REPO}"

USERDATA_FILE="$(mktemp)"
cat > "$USERDATA_FILE" <<CLOUDINIT
#!/bin/bash
set -eo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y --no-install-recommends \
  ca-certificates curl git jq \
  build-essential pkg-config libssl-dev clang mold \
  sqlite3 zstd
# Create a dedicated non-root user for the runner.
useradd -m -s /bin/bash runner 2>/dev/null || true
mkdir -p /home/runner/actions-runner
chown runner:runner /home/runner/actions-runner
cd /home/runner/actions-runner
# Download the runner binary (arch-aware).
ARCH=\$(uname -m)
[ "\$ARCH" = "x86_64" ] && RARCH=x64 || RARCH=arm64
VER=\$(curl -fsSL https://api.github.com/repos/actions/runner/releases/latest \
  | jq -r .tag_name | sed 's/^v//')
curl -fsSL -o runner.tar.gz \
  "https://github.com/actions/runner/releases/download/v\${VER}/actions-runner-linux-\${RARCH}-\${VER}.tar.gz"
tar xzf runner.tar.gz
chown -R runner:runner /home/runner/actions-runner
# Configure as ephemeral: runner deregisters from GitHub after one job.
sudo -u runner /home/runner/actions-runner/config.sh \
  --unattended \
  --url "${REPO_URL}" \
  --token "${REG_TOKEN}" \
  --name "${RUNNER_NAME}" \
  --labels "self-hosted,linux,${RUNNER_LABEL}" \
  --work "_work" \
  --ephemeral
# Start in background so cloud-init exits and the runner picks up the job.
nohup sudo -u runner /home/runner/actions-runner/run.sh \
  >> /var/log/actions-runner.log 2>&1 &
CLOUDINIT

# ── Find Ubuntu 22.04 image in this region ────────────────────────────────
log "looking up Ubuntu 22.04 image in ${OS_REGION_NAME}..."
IMAGE_ID=$(openstack image list --name "Ubuntu 22.04" -f value -c ID 2>/dev/null | head -1)
if [ -z "${IMAGE_ID:-}" ]; then
  # Fallback: fuzzy match (OVHCloud sometimes adds suffixes)
  IMAGE_ID=$(openstack image list -f value -c ID -c Name 2>/dev/null \
    | grep -i "ubuntu.*22\.04" | head -1 | awk '{print $1}')
fi
if [ -z "${IMAGE_ID:-}" ]; then
  echo "ERROR: Ubuntu 22.04 image not found in ${OS_REGION_NAME}" >&2
  log "Available images:"
  openstack image list 2>/dev/null | head -20 >&2
  rm -f "$USERDATA_FILE"
  exit 1
fi
log "image ID: ${IMAGE_ID}"

# ── Create the instance ───────────────────────────────────────────────────
log "creating instance (flavor=${FLAVOR}, region=${OS_REGION_NAME})..."
log "runner will register as '${RUNNER_NAME}' with label '${RUNNER_LABEL}'"

INSTANCE_ID=$(openstack server create \
  --flavor   "${FLAVOR}" \
  --image    "${IMAGE_ID}" \
  --user-data "$USERDATA_FILE" \
  --wait \
  -f value -c id \
  "${RUNNER_NAME}")
rm -f "$USERDATA_FILE"

log "instance created: ${INSTANCE_ID}"

# Write outputs BEFORE waiting for the runner so teardown has the ID even if
# the wait below times out.
echo "instance_id=${INSTANCE_ID}"  >> "$GITHUB_OUTPUT"
echo "runner_label=${RUNNER_LABEL}" >> "$GITHUB_OUTPUT"

# ── Wait for the runner to register ──────────────────────────────────────
log "waiting for runner '${RUNNER_NAME}' to come online (up to 5 min)..."
for i in $(seq 1 30); do
  STATUS=$(curl -fsSL \
    -H "Authorization: Bearer ${RUNNER_PAT}" \
    -H "Accept: application/vnd.github+json" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    "https://api.github.com/repos/${GH_REPO}/actions/runners?per_page=50" \
    | jq -r ".runners[] | select(.name == \"${RUNNER_NAME}\") | .status" 2>/dev/null \
    || true)
  if [ "${STATUS:-}" = "online" ]; then
    log "runner online — bench job can proceed"
    exit 0
  fi
  log "  attempt ${i}/30 — status: ${STATUS:-not yet registered}"
  sleep 10
done

echo "ERROR: runner '${RUNNER_NAME}' did not come online within 5 minutes" >&2
echo "Check /var/log/actions-runner.log on instance ${INSTANCE_ID}" >&2
exit 1
