#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${COMPACT_INSTALL_DIR:-$HOME/.local/compact-server}"
SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${COMPACT_PORT:-8080}"
# Nixery OCI registry (e.g. https://nixery.ts-node or https://nixery.your-tailnet).
# Leave unset to use system Python via uv.
NIXERY_REGISTRY="${NIXERY_REGISTRY:-}"
# Image path components for the Python runtime (Nixery builds the image on demand).
NIXERY_PYTHON_IMAGE="${NIXERY_PYTHON_IMAGE:-python314t/uv}"

echo "Installing compact-server to $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"
cp "$SCRIPT_DIR"/{server.py,compress.py,enrich.py,pyproject.toml} "$INSTALL_DIR/"
mkdir -p "$INSTALL_DIR/tests"
cp "$SCRIPT_DIR/tests/"*.py "$INSTALL_DIR/tests/" 2>/dev/null || true

if [ -n "$NIXERY_REGISTRY" ]; then
    IMAGE="$NIXERY_REGISTRY/$NIXERY_PYTHON_IMAGE"
    echo "Pulling Nixery Python image: $IMAGE"
    docker pull "$IMAGE"

    echo "Pre-downloading LLMLingua-2 model (~500MB on first run)..."
    docker run --rm \
        -v "$INSTALL_DIR:/app" -w /app \
        "$IMAGE" \
        uv run python -c "
from llmlingua import PromptCompressor
PromptCompressor('microsoft/llmlingua-2-xlm-roberta-large-meetingbank', use_llmlingua2=True, device_map='cpu')
print('LLMLingua-2 ready')
"
    cat > "$INSTALL_DIR/start.sh" <<EOF
#!/usr/bin/env bash
exec docker run --rm \\
    -v $INSTALL_DIR:/app -w /app \\
    -p $PORT:$PORT -e COMPACT_PORT=$PORT \\
    $IMAGE \\
    uv run uvicorn server:app --host 0.0.0.0 --port $PORT
EOF
else
    echo "Installing dependencies with uv..."
    uv --directory "$INSTALL_DIR" sync

    echo "Pre-downloading LLMLingua-2 model (~500MB on first run)..."
    uv --directory "$INSTALL_DIR" run python -c "
from llmlingua import PromptCompressor
PromptCompressor('microsoft/llmlingua-2-xlm-roberta-large-meetingbank', use_llmlingua2=True, device_map='cpu')
print('LLMLingua-2 ready')
"
    cat > "$INSTALL_DIR/start.sh" <<EOF
#!/usr/bin/env bash
exec uv --directory $INSTALL_DIR run uvicorn server:app --host 0.0.0.0 --port $PORT
EOF
fi
chmod +x "$INSTALL_DIR/start.sh"

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
        <string>$INSTALL_DIR/start.sh</string>
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
ExecStart=$INSTALL_DIR/start.sh
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
