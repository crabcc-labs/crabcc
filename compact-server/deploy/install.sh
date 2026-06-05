#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${COMPACT_INSTALL_DIR:-$HOME/.local/compact-server}"
SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${COMPACT_PORT:-8080}"

echo "Installing compact-server to $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"
cp "$SCRIPT_DIR"/{server.py,compress.py,enrich.py,pyproject.toml} "$INSTALL_DIR/"
mkdir -p "$INSTALL_DIR/tests"
cp "$SCRIPT_DIR/tests/"*.py "$INSTALL_DIR/tests/" 2>/dev/null || true

python3 -m venv "$INSTALL_DIR/.venv"
"$INSTALL_DIR/.venv/bin/pip" install -e "$INSTALL_DIR" --quiet

echo "Pre-downloading LLMLingua-2 (first run: ~500MB)..."
"$INSTALL_DIR/.venv/bin/python3" -c "
from llmlingua import PromptCompressor
PromptCompressor('microsoft/llmlingua-2-xlm-roberta-large-for-general-compression', use_llmlingua2=True, device_map='cpu')
print('LLMLingua-2 ready')
"

if [[ "$(uname)" == "Darwin" ]]; then
    PLIST="$HOME/Library/LaunchAgents/cc.crabcc.compact-server.plist"
    cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>       <string>cc.crabcc.compact-server</string>
    <key>ProgramArguments</key>
    <array>
        <string>$INSTALL_DIR/.venv/bin/python3</string>
        <string>$INSTALL_DIR/server.py</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict><key>COMPACT_PORT</key><string>$PORT</string></dict>
    <key>RunAtLoad</key>   <true/>
    <key>KeepAlive</key>   <true/>
    <key>StandardOutPath</key>  <string>$HOME/.crabcc/compact-server.log</string>
    <key>StandardErrorPath</key> <string>$HOME/.crabcc/compact-server.err</string>
</dict>
</plist>
PLIST
    launchctl load "$PLIST" && echo "Loaded launchd service on port $PORT"
else
    UNIT="$HOME/.config/systemd/user/compact-server.service"
    mkdir -p "$(dirname "$UNIT")"
    cat > "$UNIT" <<UNIT
[Unit]
Description=crabcc compact-server
After=network.target
[Service]
ExecStart=$INSTALL_DIR/.venv/bin/python3 $INSTALL_DIR/server.py
Environment=COMPACT_PORT=$PORT
Restart=always
StandardOutput=append:$HOME/.crabcc/compact-server.log
StandardError=append:$HOME/.crabcc/compact-server.err
[Install]
WantedBy=default.target
UNIT
    systemctl --user daemon-reload && systemctl --user enable --now compact-server
    echo "Enabled systemd user service on port $PORT"
fi
