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

## Memory layer — current state

`.crabcc/memory.db` stores per-repo notes. Issue #2's epic is closed
end-to-end:

- **Storage** — `SqliteBackend` (WAL + FSST drawer-body compression)
  and an optional `sqlite-vec` ANN scaffold behind `--features
  memory-vec`.
- **Hybrid search** — FTS5 BM25 ⊕ cosine KNN fused via Reciprocal
  Rank Fusion (k = 60). `crabcc memory search QUERY [--mode lexical|vector|hybrid]`.
- **Real embeddings** — `FastEmbedder` (MiniLM-L6-v2, 384-dim) behind
  `--features memory-embed` (~25 MB ONNX, lazy-downloaded into
  `~/.cache/crabcc-memory/` on first use).
- **Miners** — `crabcc memory mine project [PATH]` for repo files,
  `crabcc memory mine sessions [DIR]` for Claude Code JSONL
  transcripts. Both idempotent.
- **Bench gate** — `task memory-bench` runs the LongMemEval R@k
  harness in `bench/memory/` against a bundled 12-question synthetic
  fixture; clears R@5 ≥ 96.6% under `lexical` and `hybrid` modes.
  Real LongMemEval requires `DATASET=path/to/longmemeval_oracle.json`.

CLI surface: `init`, `remember`, `search`, `get`, `list`, `delete`,
`forget`, `count`, `health`, `mine {project,sessions}`. MCP exposes
matching `memory.*` tools (10 in total).

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
| Memory bench (R@5 gate) | `task memory-bench` (or `task memory-bench DATASET=...`) |
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
2. Per-crate deep-dives (these stay in-tree — the `docs/` *inside* each crate is tracked):
   - [`crates/crabcc-core/docs/HOW_IT_WORKS.md`](./crates/crabcc-core/docs/HOW_IT_WORKS.md) — library + internals reference (extractor, parser pool, schema).
   - [`crates/ucracc-lsp/docs/HOW_IT_WORKS.md`](./crates/ucracc-lsp/docs/HOW_IT_WORKS.md) — LSP user + developer reference.
3. The repo-root `docs/` is a **private submodule** (`peterlodri-sec/crabcc-docs`) — architecture notes,
   roadmap, and research live there. To populate locally:
   ```
   git submodule update --init docs
   ```
   The previous `docs/ARCHITECTURE.md`, `docs/ROADMAP-v2.5.md`, `docs/RESEARCH-mempalace.md`, etc. are
   inside that submodule.
