# crabcc integrations

Config fragments and installers for coding agents, orchestration stacks, and OS-native surfaces.

Install everything (or pick targets):

```bash
crabcc setup install-integrations --target all --yes
crabcc setup install-integrations --target cursor,gemini,opencode --project
```

| Target | What it wires |
|--------|----------------|
| `cursor` | Skill symlink, project `.mcp.json`, hooks under `.cursor/hooks/` |
| `claude` | Delegates to `crabcc install-claude` (skill, commands, hooks printout) |
| `gemini` | Prints merge instructions for `~/.gemini/settings.json` |
| `opencode` | Prints merge for `~/.config/opencode/opencode.json` |
| `langchain` | Materializes Python LangChain/LangGraph examples under `~/.crabcc/integrations/langchain/` |
| `os` | systemd user unit + launchd plist templates under `~/.crabcc/integrations/os/` |
| `kernel` | Bleeding-edge kernel config fragment + build instructions |

Fragments in this directory are embedded in the `crabcc` binary — no checkout required after `cargo install`.

See also: [`install/integrations.md`](../integrations.md) (full guide).
