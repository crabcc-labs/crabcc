# Integrations guide

One installer wires crabcc into coding agents, orchestration stacks, and OS services.

```bash
crabcc setup install-integrations --target all --yes
crabcc setup install-integrations --target cursor,langchain --project
```

## Coding agents

### Cursor

| Surface | Location |
|---------|----------|
| MCP | `~/.cursor/mcp.json` or project `.cursor/mcp.json` |
| Skill | `~/.cursor/skills/crabcc/SKILL.md` |
| Hooks | `.cursor/hooks.json` + `.cursor/hooks/crabcc-hint.sh` |

```bash
crabcc setup install-integrations --target cursor --project --yes
```

Restart Cursor after MCP changes. Enable the `crabcc` server under **Settings → MCP**.

### Claude Code

```bash
crabcc install-claude --yes
# or
crabcc setup install-integrations --target claude --yes
```

Registers skill + slash commands; prints `claude mcp add crabcc -- crabcc --mcp` and hook JSON.

### Gemini CLI

Merge `install/integrations/gemini-settings.fragment.json` into:

- User: `~/.gemini/settings.json`
- Project: `.gemini/settings.json`

```bash
crabcc setup install-integrations --target gemini
```

### OpenCode

Merge `install/integrations/opencode.fragment.jsonc` into:

- Global: `~/.config/opencode/opencode.json`
- Project: `opencode.json`

```bash
crabcc setup install-integrations --target opencode
```

## LangChain / LangGraph / LangSmith

```bash
crabcc setup install-integrations --target langchain --yes
cd ~/.crabcc/integrations/langchain && pip install -e .
```

- **Tools**: `crabcc_sym`, `crabcc_refs`, `crabcc_callers`, `crabcc_outline`
- **Graph**: `build_lookup_graph(model)` — agent ↔ tools loop
- **LangSmith batch eval**: `tools/orchestrator/import-dataset.sh` → queue → `upload-experiment.sh`

Set `LANGSMITH_API_KEY` and `LANGCHAIN_TRACING_V2=true` for trace export.

## OS-native

```bash
crabcc setup install-integrations --target os
```

Materializes under `~/.crabcc/integrations/os/`:

- `com.crabcc.mcp.plist` — macOS LaunchAgent (`--mcp-http` on :8091)
- `crabcc-mcp.service` — systemd user unit (Linux)
- iTerm2: `task install-iterm2`
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
