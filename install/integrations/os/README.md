# OS-native integration

Templates materialized by `crabcc setup install-integrations --target os`.

## macOS (launchd)

```bash
# After install-integrations --target os
cp ~/.crabcc/integrations/os/com.crabcc.mcp.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.crabcc.mcp.plist
crabcc doctor
```

## Linux (systemd user unit)

```bash
mkdir -p ~/.config/systemd/user
cp ~/.crabcc/integrations/os/crabcc-mcp.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now crabcc-mcp.service
```

Point agents at `http://127.0.0.1:8091` when using `--mcp-http`. Set
`MCP_AUTH_TOKEN` in the unit file for non-loopback binds.

## iTerm2 HUD

```bash
task install-iterm2
crabcc doctor iterm2
```

See [`apps/crabcc-iterm2/README.md`](../../../apps/crabcc-iterm2/README.md).
