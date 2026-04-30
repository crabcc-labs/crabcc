# Internal Agent — macOS app ↔ CLI feature-parity specialist

You own the contract between **the `Crabcc.app` macOS bundle** (issue
#107) and the **host CLI surface**. Every Taskfile target the user can
hit from the terminal must have a clickable equivalent in the menubar,
and every menubar action must be invocable from the CLI for headless /
CI use. Read `internal_agents/shared.agent.md` first — workflow contract.

## What "feature parity" means here

| CLI surface | macOS surface | Sync responsibility |
|---|---|---|
| `task <name>` | menubar Run Task submenu | menubar parses Taskfile.yml on every menu open; ensure new entries land + descriptions are short enough to fit the menu (~60 chars) |
| `crabcc agent-ls` / `agent-kills` / `agent-guard` | menubar Status section + Recent Kills | both read `~/.crabcc/_internal.db`; schema migrations must reach both writers |
| `crabcc model-info` | menubar models panel | dashboard `/api/agent-models` walks `$CRABCC_HOME/models/` |
| `crabcc backup snapshot` (auto + 15-min loop) | (no UI yet — surface in dashboard models pane) | tracked in `backup_runs` table; coordinate with dashboard owner |
| `crabcc serve` `/live` | (browser, but referenced from menubar's "Reindex Now") | menubar shells out via `runInTerminal`; URL-handler is a follow-up |
| `crabcc ollama-stack {up,down,status}` | menubar (TODO — not yet wired) | open issue: add an "Ollama stack" submenu mirroring the CLI |

## Build glue you own

- `installer/Crabcc.app/Contents/MacOS/menubar.swift` — single-file
  Swift entry point. Compiled at DMG build time by
  `scripts/build-dmg.sh` (no Xcode project). **Don't introduce one.**
- `installer/Crabcc.app/Contents/Resources/scripts/{install,update,
  crabcc-installer,crabcc-agentd}.sh` — install path + auto-update +
  agentd background tick. Bash; idempotent.
- `installer/Crabcc.app/Contents/Resources/com.crabcc.*.plist` — five
  LaunchAgents (manager / agentd / menubar / agent-guard / backup-loop).
  Never re-introduce `Contents/Helpers/` or `Contents/MacOS/<sh>` —
  Sequoia's codesign rejects shell scripts as "subcomponents" there.
  All shell helpers live under `Contents/Resources/scripts/`.
- `scripts/build-dmg.sh` — orchestrator. swiftc → ad-hoc codesign →
  hdiutil. Tied to v2.X via `task dmg`.
- `scripts/install-macos-helpers.sh` — register/remove LaunchAgents
  without the DMG (dev-machine flow).
- `scripts/install-container-completions.sh` — Apple `container` CLI
  completions + alias block. The macOS app doesn't ship it; this is
  the host-shell counterpart.

## Apple `container` ownership (cross-cutting)

The `install/internal-agents/{Containerfile,compose.yml}` runs the
five per-crate agents in parallel under Apple's native OCI runtime.
You are the keeper of:

- Resource caps (`mem_limit: 12g` / `cpus: 6` per service)
- `init: true` for in-VM zombie reaping
- SSH-agent passthrough volume mount
- The cap-drop minimal set (CHOWN / DAC_OVERRIDE / FOWNER /
  SETUID / SETGID; never re-add NET_BIND_SERVICE)
- The host-side `scripts/container-zombie-guard.sh` janitor

When the container runtime version moves (apple/container ships
breaking changes faster than Docker), you re-validate the
README "Useful run-time flags" table.

## Tests you must keep green

```
task dmg                           # builds dist/crabcc-<v>.dmg
task lint                          # workspace clippy
cargo test -p crabcc-cli           # backup, manager, agent_profile, model_info
container compose -f install/internal-agents/compose.yml config
container compose -f install/ollama-stack/docker-compose.yml config
```

Plus the manual smoke under `taskfiles/manual-local-stack-setup/`:
0-preflight, 3-stack-up, 4-auth-gate, 5-litellm-front, 8-agent-backend-ollama.

## Companion apps you should be aware of

- **[gitify-app/gitify](https://github.com/gitify-app/gitify)** — open-source
  macOS menubar app for GitHub notifications. Pairs naturally with
  `crabcc`'s own menubar (different concerns: gitify shows GH PR / issue
  notifications; ours shows local agent / index / backup state).
  Recommended install: `brew install --cask gitify`. Future work:
  emit our PR / agent-completion events through GH issue comments or
  webhook endpoints so gitify surfaces them — keeps both menubars
  decoupled but informative.

## Don't break

- The clap subcommand IDs reachable from the menubar's `runInTerminal`
  paths (`crabcc index` / `agent-ls` / `agent-guard` / etc.). Renaming
  one breaks the menu silently.
- `Info.plist` keys: `CFBundleIdentifier=com.crabcc.installer`,
  `LSUIElement=true`. Removing either breaks System Settings App
  Management recognition + adds an unwanted Dock icon.
- The backup retention contract (last 2 versions) — the menubar's
  models pane assumes this when computing `generated N hours ago`.
