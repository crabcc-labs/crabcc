# Integrations guide

v4.5 narrows the integration surface to two agents:

- **Claude Code** — the "big" example (full MCP server + slash commands + hooks)
- **pi** — the "tiny" example (single SKILL.md, skills array config)

Cursor / Gemini / OpenCode / LangChain were removed in the sharpening release —
see CHANGELOG.

```bash
crabcc setup install-integrations --target all --yes
crabcc setup install-integrations --target claude --yes
crabcc setup install-integrations --target pi --yes
```

## Coding agents

### Claude Code

```bash
crabcc install-claude --yes
# or
crabcc setup install-integrations --target claude --yes
```

Registers skill + slash commands; prints `claude mcp add crabcc -- crabcc --mcp` and hook JSON.

### pi

```bash
crabcc setup install-integrations --target pi --yes              # global only
crabcc setup install-integrations --target pi --project --yes    # global + project
```

pi reads skills from `~/.pi/agent/skills/<name>/SKILL.md` (global) and
`.pi/skills/<name>/SKILL.md` (project) and enables them via the `skills` array
in `settings.json`. The installer symlinks `skill/crabcc/SKILL.md` into the
right place and prints the settings fragment to merge:

```json
{
  "skills": ["crabcc"]
}
```

Merge into `~/.pi/agent/settings.json` (global) or `.pi/settings.json` (project).

> pi does not currently expose a native MCP server config (per
> [pi.dev/docs/latest/settings](https://pi.dev/docs/latest/settings) —
> `skills` and `extensions` are the supported registration paths). When pi
> grows MCP support we'll route crabcc through that channel; for now the
> skill provides the integration surface.

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
