# CLAUDE.md

> Claude Code-specific notes for `crabcc`. Tool-agnostic agent guidance —
> rules, conventions, workspace layout — lives in [`AGENTS.md`](./AGENTS.md);
> read that first. This file only adds what's specific to running inside
> Claude Code.

## Use crabcc, not grep

This repo is the source of `crabcc` itself. **Eat your own dogfood.** Never
reach for `grep -rn`, `find . -name`, or `rg "ClassName"` when a `crabcc`
subcommand answers the question in fewer tokens.

## Line-by-line: navigating this repo with crabcc

```bash
# 1. One-time index (~5–30 s on a 13 k-file repo):
crabcc index

# 2. Where is `Store` defined?
crabcc sym Store
# → JSON: {name, kind, signature, file, line_start, line_end, parent, …}

# 3. What calls `Store::open`?
crabcc callers Store::open
# → list of {file, line, snippet} hits.

# 4. Just the count? (–99.98 % bytes vs full hits)
crabcc callers Store::open --count
# → {"count": 17}

# 5. Just the deduped file list? (–99.6 % bytes)
crabcc refs Store --files-only --limit 20
# → {"files": ["crates/crabcc-core/src/store.rs", …]}

# 6. Outline a file before reading the whole thing:
crabcc outline crates/crabcc-core/src/store.rs
# → every fn / struct / impl with line ranges.

# 7. Misremembered the name? Levenshtein-2 fuzzy:
crabcc fuzzy strore        # finds "store"

# 8. List indexed source files (replaces `ls -R` / `find`):
crabcc files --under crates/crabcc-core/src --ext rs --limit 50

# 9. Call-graph queries (built from the populated `edges` table):
crabcc graph build         # one-shot SQL scan, O(files)
crabcc graph walk Store::open --dir callers --depth 3
crabcc graph cycles        # SCCs of size ≥ 2
crabcc graph orphans       # defined fns with no incoming callers

# 10. Per-repo memory drawer (M0 + M3-light surface):
crabcc memory remember "doc:1" "<note body>"
crabcc memory search "<query>"        # KEYWORD only today; semantic in v2.5
crabcc memory list --limit 20

# 11. Token-savings ledger:
crabcc track
```

The same surface is exposed as the **`crabcc` MCP server** — every CLI
subcommand has a matching tool. Wire it up with:

```bash
claude mcp add crabcc -- crabcc --mcp
# or, paste-ready: `crabcc install-claude` (also symlinks the skill +
# slash command into ~/.claude/, prints SessionStart + PreToolUse hook
# templates without modifying any global Claude config).
```

## Slash commands & skill

- `/crabcc-init` — bootstrap the index in a fresh worktree.
- `/crabcc-upgrade` — check the GitHub repo for a newer release (added in v2.1.0).
- Skill at `skill/crabcc/SKILL.md` — auto-routes grep / find-shaped questions
  to the right `crabcc` subcommand. Symlinked into `~/.claude/skills/crabcc/`
  by `crabcc install-claude`.

## Memory layer — current limits

`.crabcc/memory.db` stores per-repo notes. Today it ships **M0 + M3-light**:
the persistent `SqliteBackend`, the `Palace` facade, the `PalaceRegistry`
session router, the full CLI surface (`init`, `remember`, `search`, `get`,
`list`, `delete`, `count`, `health`), and 8 matching `memory.*` MCP tools.

> **Search is keyword-only today.** M0 ships a deterministic-hash
> `Embedder` so the API works end-to-end and tests are stable. Real
> semantic search lands in v2.5 via `sqlite-vec` (M0.5) and `fastembed-rs`
> (M1). See [`docs/ROADMAP-v2.5.md`](./docs/ROADMAP-v2.5.md).

Set `CRABCC_AUTO_MEMORY=1` to have query-shaped commands (`sym` / `refs` /
`callers` / `fuzzy` / `prefix`) silently capture a drawer per call.

## Building / testing — daily-driver targets

| Goal | Command |
|---|---|
| Build + test | `task` |
| Release build only | `task build` (LTO=fat) |
| Faster iteration | `task build-fast` (`-O1`) |
| Format gate | `task fmt-check` |
| Lint gate | `task lint` (clippy `-D warnings`) |
| Local CI dry-run | `task ci` |
| Symbol-index smoke | `task smoke` |
| Memory CLI smoke | `task memory-smoke` |
| FSST gate bench | `task bench-compress REPO_FIXTURE=/path/to/big-repo` |
| **Token-minimized repo bundle** | **`task repomix`** → `.repomix/crabcc.xml` |
| Cut a release | `task release VERSION=x.y.z` |

## When changing schema

Schema is **additive**. Never `DROP COLUMN`. Pattern: add column +
idempotent `ALTER` in `Store::open` (mirrored in
`crabcc-memory/schema/`). See AGENTS.md → "Conventions agents should
respect" for the full list.

## Where to read next

1. [`AGENTS.md`](./AGENTS.md) — agent rules + workspace layout. **Start here.**
2. [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) — data model + indexing pipeline.
3. [`docs/ROADMAP-v2.5.md`](./docs/ROADMAP-v2.5.md) — what's coming next.
4. [`docs/RESEARCH-mempalace.md`](./docs/RESEARCH-mempalace.md) — memory-layer port plan.
5. [`docs/RESEARCH-fsst.md`](./docs/RESEARCH-fsst.md) — FSST integration design.
