#!/usr/bin/env bash
# Deploy the Nixery OCI registry to a remote node (default: m3-task).
# Nixery serves custom Nix-built Docker images on demand.
# Our overlay adds python315t-optimized (PGO + ThinLTO + JIT + free-threaded).
#
# Usage:
#   ./scripts/deploy-nixery.sh
#   NIXERY_REMOTE=my-node NIXERY_PORT=5000 ./scripts/deploy-nixery.sh
#
# After deploy, use from compact-server install:
#   NIXERY_REGISTRY=<remote>:5000 \
#   NIXERY_PYTHON_IMAGE=python315t-optimized/uv \
#     bash compact-server/deploy/install.sh
set -euo pipefail

REMOTE="${NIXERY_REMOTE:-m3-task}"
NIXERY_PORT="${NIXERY_PORT:-5000}"
REMOTE_DIR="/opt/plodri/nixery"
LOCAL_NIX="$(cd "$(dirname "$0")/../nix" && pwd)"

echo "Syncing nix/ overlay to $REMOTE:$REMOTE_DIR ..."
ssh "$REMOTE" "mkdir -p $REMOTE_DIR"
rsync -av --delete "$LOCAL_NIX/" "$REMOTE:$REMOTE_DIR/"

echo "Starting Nixery on $REMOTE:$NIXERY_PORT ..."
# Pass vars as env; heredoc is single-quoted so no local expansion inside.
ssh "$REMOTE" \
    NIXERY_PORT="$NIXERY_PORT" \
    REMOTE_DIR="$REMOTE_DIR" \
    bash <<'REMOTE'
set -euo pipefail

PKGS_PATH="$REMOTE_DIR/nixery-pkgs"
CACHE_DIR="/var/cache/nixery"
sudo mkdir -p "$CACHE_DIR"

if command -v docker &>/dev/null; then
    echo "Starting Nixery via Docker ..."
    docker rm -f nixery 2>/dev/null || true
    docker run -d --name nixery --restart unless-stopped \
        -p "$NIXERY_PORT:8080" \
        -v "$PKGS_PATH:/nixery-pkgs:ro" \
        -v "$CACHE_DIR:/var/cache/nixery" \
        -e NIXERY_PKGS_PATH=/nixery-pkgs \
        -e NIXERY_STORAGE_BACKEND=filesystem \
        -e NIXERY_PKGS_CACHE=/var/cache/nixery \
        gcr.io/nixery/nixery
    echo "Nixery running (Docker) at http://localhost:$NIXERY_PORT"

elif command -v nix &>/dev/null; then
    # Write a launchd plist (macOS) or systemd unit (Linux).
    if [[ "$(uname)" == "Darwin" ]]; then
        PLIST="$HOME/Library/LaunchAgents/cc.crabcc.nixery.plist"
        # Nixery binary from flake; nix run is slow on first launch — build first.
        nix build github:tazjin/nixery --out-link "$HOME/.local/nixery"
        NIXERY_BIN="$HOME/.local/nixery/bin/nixery"
        cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>           <string>cc.crabcc.nixery</string>
    <key>ProgramArguments</key>
    <array>
        <string>$NIXERY_BIN</string>
        <string>serve</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>NIXERY_PKGS_PATH</key>    <string>$PKGS_PATH</string>
        <key>NIXERY_STORAGE_BACKEND</key> <string>filesystem</string>
        <key>NIXERY_PKGS_CACHE</key>   <string>$CACHE_DIR</string>
        <key>PORT</key>                <string>$NIXERY_PORT</string>
    </dict>
    <key>RunAtLoad</key>   <true/>
    <key>KeepAlive</key>   <true/>
    <key>StandardOutPath</key>  <string>$HOME/.crabcc/nixery.log</string>
    <key>StandardErrorPath</key><string>$HOME/.crabcc/nixery.err</string>
</dict>
</plist>
PLIST
        launchctl load "$PLIST"
        echo "Nixery running (launchd) at http://localhost:$NIXERY_PORT"
    else
        UNIT="$HOME/.config/systemd/user/nixery.service"
        mkdir -p "$(dirname "$UNIT")"
        nix build github:tazjin/nixery --out-link "$HOME/.local/nixery"
        NIXERY_BIN="$HOME/.local/nixery/bin/nixery"
        cat > "$UNIT" <<UNIT
[Unit]
Description=Nixery OCI registry
After=network.target
[Service]
ExecStart=$NIXERY_BIN serve
Environment=NIXERY_PKGS_PATH=$PKGS_PATH
Environment=NIXERY_STORAGE_BACKEND=filesystem
Environment=NIXERY_PKGS_CACHE=$CACHE_DIR
Environment=PORT=$NIXERY_PORT
Restart=always
[Install]
WantedBy=default.target
UNIT
        systemctl --user daemon-reload && systemctl --user enable --now nixery
        echo "Nixery running (systemd) at http://localhost:$NIXERY_PORT"
    fi
else
    echo "ERROR: neither docker nor nix found on remote — install one of them first."
    exit 1
fi
REMOTE

echo ""
echo "NOTE: python315t-optimized requires the hash in nix/overlays/python-optimized.nix."
echo "Get it with:"
echo "  nix-prefetch-github --owner python --repo cpython --rev v3.15.0b2"
echo ""
echo "Then build (first build ~45 min due to PGO profile run):"
echo "  NIXERY_REGISTRY=$REMOTE:$NIXERY_PORT \\"
echo "  NIXERY_PYTHON_IMAGE=python315t-optimized/uv \\"
echo "    bash compact-server/deploy/install.sh"
