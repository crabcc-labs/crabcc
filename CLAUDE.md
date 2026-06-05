# CLAUDE.md

> Claude Code-specific notes for `crabcc`. Tool-agnostic agent guidance —
> rules, conventions, workspace layout — lives in [`AGENTS.md`](./AGENTS.md);
> read that first. **Architecture diagrams:** [`docs/OVERVIEW.md`](./docs/OVERVIEW.md)
> (regenerate with `/crabcc-generate-overview`). This file only adds what's
> specific to running inside Claude Code.

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

# 9b. After an edit: which tests exercise the change? (verify, cheaply)
crabcc affected                       # working-tree diff -> selected tests + cmd
crabcc affected --since origin/main   # a git range instead of the working tree
crabcc affected --symbol Store::open  # explicit symbol, skip git
crabcc affected --run                 # also execute the selected tests
# → {"changed_symbols":[…],"tests":[…],"runner":"cargo",
#    "command":"cargo test -- open_wal store_roundtrip"}

# 10. Per-repo memory drawer (M0 + M3-light surface):
crabcc memory remember "doc:1" "<note body>"
crabcc memory search "<query>"        # KEYWORD only today; semantic in v2.5
crabcc memory list --limit 20

# 11. Token-savings ledger:
crabcc track

# 12. Cross-repo (v4.5+): query every indexed repo at once.
crabcc --workspace lookup sym Store
crabcc --workspace lookup fuzzy strore
crabcc --workspace lookup prefix Stor
# → {"workspace": true, "queried_repos": N, "total_hits": M, "by_repo": [...]}
# Discovers $CRABCC_HOME/repos/*/ via filesystem walk (no manifest).
# `--workspace` is mutually exclusive with `--root`.
# v4.5 ops: sym, fuzzy, prefix. refs/callers/graph walk → v5.
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

- **Storage** — `SqliteBackend` (WAL + FSST drawer-body compression).
  `sqlite-vec` ANN extension auto-loads on `Backend::open` (the
  `memory-vec` feature is **default since v3.0.0-rc.4** — opt out with
  `default-features = false` if you want the brute-force cosine path).
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
3. Repo-root `docs/` is in-tree (not a submodule anymore — see commit history). Notable contents:
   - [`docs/desktop/ARCHITECTURE.md`](./docs/desktop/ARCHITECTURE.md) and [`DESIGN-BRIEF.md`](./docs/desktop/DESIGN-BRIEF.md) — `crabcc-desktop` design + architecture.
   - [`docs/RUST-ANTHOLOGY.md`](./docs/RUST-ANTHOLOGY.md) — Rust patterns reference.
   - [`docs/PROCESS-SPAWNING.md`](./docs/PROCESS-SPAWNING.md) — agent process management notes.
   - `docs/RESEARCH-tts-voice-control-*.md` — TTS dossiers (foss / 2026 candidates).
   - `docs/desktop/design-refs/` — UI mockup PNGs (~2.6 MB, reference material for the desktop redesign).
