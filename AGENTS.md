# AGENTS.md

> [agents.md](https://agents.md/) instructions for AI coding agents working in
> this repo. Tool-agnostic: Claude Code, Cursor, Aider, Continue, any LLM editor.

## What this repo is

`crabcc` — a Rust CLI + MCP server that indexes a repo's symbols (functions,
classes, methods) for symbol-aware lookups. SQLite-backed (`.crabcc/index.db`),
with in-memory fuzzy/prefix search (no sidecar) and optional FSST signature
compression (default-on since v2.0.0-alpha). The `crabcc-memory` crate adds a
per-repo AI memory layer at `$CRABCC_HOME/repos/<slug>-<hash6>/memory.db`.

Use it instead of `grep`/`find` for symbol queries: `crabcc lookup sym Foo`,
`crabcc lookup refs Foo`, `crabcc lookup callers handleAuth`.

| Doc | Use when |
|-----|----------|
| [`docs/OVERVIEW.md`](docs/OVERVIEW.md) | **Start here** — architecture, query router, disk layout |
| [`README.md`](README.md) | Install, usage, bench numbers |
| [`crates/crabcc-core/docs/HOW_IT_WORKS.md`](crates/crabcc-core/docs/HOW_IT_WORKS.md) | Schema, extract pipeline, adding languages |
| [`crates/crabcc-memory/src/palace.rs`](crates/crabcc-memory/src/palace.rs) | Memory-layer internals + routing |

## Command surface

Query/index ops are under `crabcc lookup …`; agent ops under `crabcc agent …`.

| You want to… | Do this |
|---|---|
| Bootstrap a session | `crabcc go` (index + graph + memory + Claude `--effort max`) |
| Find a definition | `crabcc lookup sym <Name>` |
| Find references / callers | `crabcc lookup refs <Name>` / `crabcc lookup callers <Name>` |
| Outline a file | `crabcc lookup outline path/to/file.rs` |
| Store / search memory | `crabcc memory remember <src> <body>` / `crabcc memory search "<query>"` |
| List memory drawers | `crabcc memory list --limit 20` |
| Run a crabcc agent | `crabcc agent run "<task>" --backend ollama` |
| Agent run status | `crabcc agent ls --limit 5` |
| Wire agent integrations | `crabcc setup install-integrations --target all --project --yes` |

## Build & test

| Goal | Command |
|---|---|
| Build + test | `task` (= `cargo build --release && cargo test --workspace`) |
| Lint gate (CI: warnings = errors) | `task lint` |
| Format before committing | `task fmt` |
| Local CI dry-run | `task ci` |
| Symbol-index smoke | `task smoke` |
| Memory CLI smoke | `task memory-smoke` |
| Memory bench (R@5 gate) | `task memory-bench` |
| FSST gate bench | `task bench-compress REPO_FIXTURE=/path/to/big-repo` |
| Cut a release | `task release VERSION=x.y.z` |
| Bootstrap full dev env | `task setup` (uv + Ollama + model pull + stack up) |

## Code style

- **Rust 2021**, MSRV pinned in `clippy.toml` (`msrv = "1.86"`). `rustfmt` defaults.
- `clippy` is strict in CI: warnings are errors. Fix the cause; don't
  `#[allow(...)]` to silence.
- Comment the *why*, not the *what*. No docstrings added just to satisfy a linter.
- Errors: `anyhow::Result` at app boundaries; `crabcc-core` returns concrete
  `Result<T, E>` only when callers branch on the variant.
- New language extractor → `crates/crabcc-core/src/extract.rs`. Five update
  points: language enum, file-extension match, tree-sitter binding, symbol-kind
  table, and a fixture under `crates/crabcc-core/tests/fixtures/`. Walkthrough in
  `crates/crabcc-core/docs/HOW_IT_WORKS.md`.

## Conventions agents should respect

- **Schema is additive.** Never `ALTER TABLE … DROP COLUMN`. Add a column + an
  idempotent `ALTER` in `Store::open` (see how `signature_enc` landed). Same rule
  for `crates/crabcc-memory/schema/001_init.sql`.
- **Don't break the gate.** `crates/crabcc-core/src/{compress,store}.rs` are
  load-bearing. Run `task bench-compress` after touching either.
- **Reuse `crabcc-core` from `crabcc-memory`** — `walker::walk_repo`,
  `hash::sha256_hex`, the `Store::open` PRAGMA pattern, `watch::spawn`,
  `fts::Fts` already exist; don't reinvent.
- **Tests.** `crabcc-core` must pass both feature-on and `--no-default-features`;
  `crabcc-memory` stays green on `cargo test --workspace`.
- **Don't mass-rewrite imports/spacing** on files you barely touched. Let
  `task fmt` and the linter do that.
- **One feature, one PR.** Don't fold release prep, refactors, and a feature into
  one commit.
- **v4.0 index rebuild.** Opening a pre-v4 index on v4.0.0+ wipes and rebuilds it
  on the first command (~60 s on this 13k-file repo). No migrator, no opt-out;
  gated by a `schema_v4_built` meta key. Scripts should expect the first
  post-upgrade call to take rebuild-time, not query-time.

## Workspace layout

```
crates/
├── crabcc-core/    # Library: indexing, storage, query, FSST codec.
│   └── src/{compress.rs, store.rs, extract.rs, …}  benches/, fuzz/
├── crabcc-cli/     # Binary: `crabcc` (clap) + `crabcc --mcp` shim
├── crabcc-mcp/     # Library: stdio JSON-RPC 2.0 MCP server
└── crabcc-memory/  # Library: AI memory layer; Backend trait, Palace facade
bench/              # raw-bench.py (vs grep/find), compress-bench.py (FSST gate)
docs/               # In-tree docs (OVERVIEW, RUST-ANTHOLOGY, PROCESS-SPAWNING,
                    # desktop/*). Per-crate deep-dives under crates/*/docs/.
schema/001_init.sql                      # Symbol-index schema. Additive only.
crates/crabcc-memory/schema/001_init.sql # Memory schema. Additive only.
install/            # crabcc setup install-integrations templates
installer/Crabcc.app/  # macOS .app bundle (issue #107); built via `task dmg`
```

## Memory layer routing

`Palace::open(repo_root)` is idempotent — creates `.crabcc/memory.db` if missing,
reuses if present; per-git-repo by design. `PalaceRegistry` caches open palaces by
canonical git root. MCP tools take an optional `cwd` (server walks up to `.git` via
`find_git_root`); CLI uses `--root` (defaults to cwd).

Auto-capture for query-shaped commands (`sym`/`refs`/`callers`/`fuzzy`/`prefix`)
is opt-in via `CRABCC_AUTO_MEMORY=1` — off by default, zero overhead.

Bulk ingest: `crabcc memory mine project [PATH]` stores one drawer per text file
(`wing="proj"`); `crabcc memory mine sessions [DIR]` parses Claude Code JSONL
transcripts (default `$HOME/.claude/projects/`), one drawer per turn pair
(`wing="session"`). Both idempotent via the `(source_id, sha256)` UNIQUE
constraint. Hybrid search is FTS5 BM25 ⊕ cosine KNN fused via RRF (k=60);
embeddings via `--features memory-embed` (MiniLM-L6-v2, 384-dim).

## Where things live

- **Issues:** <https://github.com/peterlodri-sec/crabcc/issues>.
- **In-flight task log:** `.dev-tasks` (gitignored, local-only).
- **Bench fixtures:** mc-mothership at `/Users/peter.lodri/workspace/mc-mothership`
  (NOT in this repo; never copy or commit it). Pass via `--repo` or `REPO_FIXTURE`.
- **Skill + slash command:** `skill/crabcc/SKILL.md`, `commands/crabcc-init.md`.
  Symlink with `crabcc setup install-claude`.

## When unsure

Use `crabcc` on this repo: `crabcc lookup sym <Name>` for definitions,
`crabcc lookup outline <file>` before reading a large file,
`crabcc lookup callers <Name>` for impact analysis. Internals →
`crates/crabcc-core/docs/HOW_IT_WORKS.md`; memory specifics →
`crates/crabcc-memory/src/palace.rs` and `crates/crabcc-memory/schema/`.
