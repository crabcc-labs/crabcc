# Integrations guide

v4.5 narrows the integration surface to **Claude Code** (the "big" example)
plus the OS-native and kernel paths. pi support follows immediately after as
the "tiny" example. Cursor / Gemini / OpenCode / LangChain were removed in
the sharpening release — see CHANGELOG.

```bash
crabcc setup install-integrations --target all --yes
crabcc setup install-integrations --target claude --yes
```

## Coding agents

### Claude Code

```bash
crabcc install-claude --yes
# or
crabcc setup install-integrations --target claude --yes
```

Registers skill + slash commands; prints `claude mcp add crabcc -- crabcc --mcp` and hook JSON.

## macOS — centralised index (worktrees)

If you use Cursor worktrees or multiple checkouts, keep **one index on disk**:

```bash
source install/mac/crabcc-env.zsh   # add to ~/.zshrc
crabcc index                        # once, from any checkout
```

See `install/mac/README.md`.

## OS-native

```bash
crabcc setup install-integrations --target os
```

Materializes under `~/.crabcc/integrations/os/`:

- `com.crabcc.mcp.plist` — macOS LaunchAgent (`--mcp-http` on :8091)
- `crabcc-mcp.service` — systemd user unit (Linux)
- macOS app + agentd: `task dmg`

## Kernel (containers / custom Linux)

```bash
crabcc setup install-integrations --target kernel
install/kernel/build.sh                                    # stable 6.6
LINUX_VERSION=6.12.20 install/kernel/build.sh '' install/kernel/config.bleeding-edge.fragment
```

See [`install/kernel/README.md`](./kernel/README.md).

## MCP snippet (all agents)

```json
{
  "mcpServers": {
    "crabcc": {
      "command": "crabcc",
      "args": ["--mcp"]
    }
  }
}
```

HTTP transport (OS services): `crabcc --mcp-http 127.0.0.1:8091` with optional `MCP_AUTH_TOKEN`.
