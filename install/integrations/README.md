# crabcc integrations

Config fragments and installers for coding agents and OS-native surfaces.

Install everything (or pick targets):

```bash
crabcc setup install-integrations --target all --yes
crabcc setup install-integrations --target claude --yes
```

| Target | What it wires |
|--------|----------------|
| `claude` | Delegates to `crabcc install-claude` (skill, commands, hooks printout) |
| `pi` | Symlinks SKILL.md into `~/.pi/agent/skills/crabcc/`; prints settings fragment |
| `os` | systemd user unit + launchd plist templates under `~/.crabcc/integrations/os/` |
| `kernel` | Bleeding-edge kernel config fragment + build instructions |

Fragments in this directory are embedded in the `crabcc` binary — no checkout required after `cargo install`.

v4.5 retired the Cursor / Gemini / OpenCode / LangChain integrations. Claude
Code (big example) and pi (tiny example) are the two supported agents.

See also: [`install/integrations.md`](../integrations.md) (full guide).
