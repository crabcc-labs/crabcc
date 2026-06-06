@AGENTS.md

# CLAUDE.md

<!-- Tool-agnostic rules, command surface, conventions, and workspace layout
     live in AGENTS.md (imported above, so it loads in full every session).
     Keep THIS file to Claude-Code-specific surface only — don't duplicate
     AGENTS.md. Target <200 lines; bullets over prose; concrete + verifiable.
     Architecture diagrams: docs/OVERVIEW.md (regen: /crabcc-generate-overview). -->

Claude-Code-specific notes for `crabcc`. Everything tool-agnostic is in the
imported `AGENTS.md`; this file only adds what's specific to running here.

## Dogfood: crabcc, not grep

This repo is the source of `crabcc` itself. Use it on itself. Never reach for
`grep -rn`, `find . -name`, or `rg "ClassName"` when a `crabcc lookup`
subcommand answers in fewer tokens. The full command surface is in AGENTS.md →
"Command surface"; the [`crabcc` skill](skill/crabcc/SKILL.md) auto-routes
grep/find-shaped questions to the right subcommand.

## MCP server

Every CLI subcommand has a matching MCP tool. Wire it up:

```bash
claude mcp add crabcc -- crabcc --mcp
```

`crabcc setup install-claude` does more: installs the MCP fragment, symlinks the
skill + slash commands into `~/.claude/`, and prints SessionStart + PreToolUse
hook templates — without modifying any global Claude config.

## Slash commands

- `/crabcc-init` — bootstrap the index in a fresh worktree.
- `/crabcc-upgrade` — check GitHub for a newer release.
- `/crabcc-generate-overview` — regenerate `docs/OVERVIEW.md` diagrams.

## Settings

Set `CRABCC_AUTO_MEMORY=1` to have query-shaped commands
(`sym`/`refs`/`callers`/`fuzzy`/`prefix`) silently capture a memory drawer per
call. Off by default (see AGENTS.md → "Memory layer routing").

## When changing schema

Schema is **additive** — never `DROP COLUMN`. Add a column + idempotent `ALTER`
in `Store::open`, mirrored in `crates/crabcc-memory/schema/`. Full rule list:
AGENTS.md → "Conventions agents should respect".
