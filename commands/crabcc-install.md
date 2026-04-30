---
description: Install or upgrade crabcc from inside a Claude Code session, then verify it works.
---

Install (or upgrade in place) the `crabcc` CLI + MCP server without
leaving this Claude Code session. The script handles the full chain:
`gh auth login` if needed, clone via `gh`, `cargo install --locked`,
shell completions for the user's current shell, Claude Code skill +
slash-command symlinks, and a `crabcc go` next-step hint.

Re-running is a fast no-op when the local install matches the latest
remote release (issue #24): the script detects the existing binary,
compares versions, and skips the build step unless `--force` is
passed.

Run this:

```bash
gh api -H 'Accept: application/vnd.github.v3.raw' \
    /repos/peterlodri-sec/crabcc/contents/install.sh | bash
```

Then verify the install worked:

```bash
crabcc --version              # should print "crabcc <semver>"
crabcc info --status-line     # one-line health check (issue #43)
crabcc go                     # one-shot bootstrap of the current repo
```

If the user wants to control where the binary lands, they can pass:

- `CRABCC_INSTALL_DIR=~/.local/bin` to override the default `~/.cargo/bin`.
- `--no-completions` to skip the shell-completion step.
- `--no-claude` to skip the `~/.claude/` symlinks (rare — they're cheap).
- `--check` to dry-run: report the local-vs-remote version delta and exit.
- `--version=v2.4.0` to pin a specific tag instead of taking `main` HEAD.

After the install:

1. Confirm `crabcc --version` runs cleanly.
2. Suggest the user run `crabcc go` from a project root to bootstrap
   index + memory + open a Claude Code session with the crabcc primer
   already loaded.
3. Mention `claude mcp add crabcc -- crabcc --mcp` to register the MCP
   server (the install script prints this hint at the end too). Note
   the flag is `--mcp` (single-word), not `mcp` — the latter is the
   subcommand-style spelling that some older docs used.

## macOS extras (issue #107)

On macOS, the bootstrap can also build + install the `Crabcc.app`
menubar bundle and register the four LaunchAgents (manager / agentd /
agent-guard / menubar). The installer ad-hoc codesigns the binaries so
they run cleanly under macOS Sequoia's provenance-xattr policy.

```bash
# Fresh-machine bootstrap including the menubar app + LaunchAgents:
curl -fsSL \
    https://raw.githubusercontent.com/peterlodri-sec/crabcc/main/scripts/bootstrap.sh \
    | bash -s -- --with-launchd --with-macos-app
```

After install, `crabcc manager status` reports overall health (manager
heartbeat, each LaunchAgent's state, docker stack, active agent runs,
kill events) plus actionable recommendations on any red.
