# AGENTS.md

> [agents.md](https://agents.md/) instructions for AI coding agents working in
> this repo. Tool-agnostic: applies to Claude Code, Cursor, Aider, Continue,
> any other LLM-driven editor.

## What this repo is

`crabcc` — a Rust CLI + MCP server that indexes a repo's symbols (functions,
classes, methods, etc.) for symbol-aware lookups. SQLite-backed (`.crabcc/index.db`),
Tantivy sidecar for fuzzy/prefix search, optional FSST string compression on
the signature column (default-on as of v2.0.0-alpha).

Use it instead of `grep`/`find` for symbol-name queries: `crabcc sym Foo`,
`crabcc refs Foo`, `crabcc callers handleAuth`. See `README.md` and
`docs/ARCHITECTURE.md` for the full surface.

The `crabcc-memory` crate (epic [#2](https://github.com/peterlodri-sec/crabcc/issues/2))
adds a per-repo AI memory layer at `.crabcc/memory.db`, fronted by a `Backend`
trait, `Palace` facade, and `crabcc memory` CLI / `memory.*` MCP tools.

## Quick orientation

| You want to… | Do this |
|---|---|
| Bootstrap a session in one command | `crabcc go` (index + graph + memory + Claude with `--effort max`) |
| Find a definition | `crabcc sym <Name>` |
| Find references / call sites | `crabcc refs <Name>` / `crabcc callers <Name>` |
| Outline a file | `crabcc outline path/to/file.rb` |
| Store / search project memory | `crabcc memory remember <src> <body>` / `crabcc memory search "<query>"` |
| List drawers in this repo | `crabcc memory list --limit 20` |
| Build & test | `task` (= `cargo build --release && cargo test --workspace`) |
| Lint gate | `task lint` (= `cargo clippy --workspace --all-targets -- -D warnings`) |
| Format | `task fmt` |
| Local CI dry-run | `task ci` |
| Run the FSST bench | `task bench-compress REPO_FIXTURE=/path/to/big-repo` |
| Memory smoke (CLI surface) | `task memory-smoke` |
| Cut a release | `task release VERSION=x.y.z` |

## Code style

- **Rust 2021 edition**, MSRV pinned via `clippy.toml` (`msrv = "1.86"`, set by `fsst-rs`).
- `rustfmt` defaults — see `rustfmt.toml`. Run `task fmt` before committing.
- `clippy` strict in CI: warnings are errors. Don't `#[allow(...)]` to silence
  them; fix the underlying issue.
- Comments live where the *why* is non-obvious. Don't restate what the code
  does. Don't add docstrings just to satisfy a linter.
- Error handling via `anyhow::Result` at app boundaries; library code in
  `crabcc-core` returns concrete `Result<T, E>` only when the caller needs to
  branch on the error variant.
- New language extractors land in `crates/crabcc-core/src/extract.rs`. There
  are five entry points to update — see `docs/ARCHITECTURE.md` § Extending.

## Workspace layout

```
crates/
├── crabcc-core/          # Library: indexing, storage, query, FSST codec.
│   ├── benches/symbols.rs    # criterion micro-benches
│   ├── fuzz/                 # cargo-fuzz target for FSST round-trip
│   └── src/
│       ├── compress.rs   # FSST Codec (feature = "compress")
│       ├── store.rs      # SQLite Store + signature_enc decode helper
│       ├── extract.rs    # tree-sitter symbol extractors (TS/JS/RB/Rust/Go/Py)
│       └── …
├── crabcc-cli/           # Binary: `crabcc` (clap) + `crabcc --mcp` shim
├── crabcc-mcp/           # Library: stdio JSON-RPC 2.0 MCP server logic
└── crabcc-memory/        # Library: AI memory layer (M0+); Backend trait,
                          # Palace facade, schema/001_init.sql.

bench/                    # raw-bench.py (vs grep/find), compress-bench.py (FSST gate)
docs/
├── ARCHITECTURE.md       # Read this before touching cross-crate code.
├── RESEARCH-fsst.md      # FSST integration design + release-gate criteria.
└── RESEARCH-mempalace.md # Memory-layer port plan + roadmap (M0 → M7).

schema/001_init.sql                      # Symbol-index schema. Additive only.
crates/crabcc-memory/schema/001_init.sql # Memory schema (wings/rooms/drawers/…).
install/                  # crabcc install-claude templates (hooks-claude.json)
```

## Conventions agents should respect

- **Don't break the gate.** `crates/crabcc-core/src/compress.rs` and
  `crates/crabcc-core/src/store.rs` are load-bearing for the v2.0.0-alpha
  release. Run `task bench-compress` after changes to either.
- **Schema is additive.** Never `ALTER TABLE … DROP COLUMN`. The pattern is
  add column + idempotent `ALTER` in `Store::open` (see how `signature_enc`
  was landed). Same rule for `crabcc-memory/schema/001_init.sql`.
- **Reuse `crabcc-core` from `crabcc-memory`.** `walker::walk_repo`,
  `hash::sha256_hex`, the `Store::open` PRAGMA pattern, `watch::spawn`, and
  `fts::Fts` are already there — don't reinvent.
- **Tests.** Both feature-on and feature-off must pass for `crabcc-core`
  (`cargo test -p crabcc-core` / `cargo test -p crabcc-core --no-default-features`).
  `crabcc-memory` tests must stay green on `cargo test --workspace`.
- **Don't mass-rewrite imports / spacing on files you barely touched.** The
  linter and `task fmt` keep things consistent — let them do their job.
- **One feature, one PR.** Don't fold release prep, refactors, and a feature
  into a single commit.

## Memory layer routing

`Palace::open(repo_root)` is idempotent — creates `.crabcc/memory.db` if
missing, reuses if present. Per-git-repo by design.

`PalaceRegistry` caches open palaces by canonical git root. MCP tools accept
an optional `cwd` arg; the server walks up to `.git` via `find_git_root` and
routes the call to the right palace. CLI uses `--root` (defaults to cwd).

`session_id` propagation: pass `$TERM_SESSION_ID` (CLI) or a conversation id
(MCP) to drawer rows so later queries can group by invocation. The
`SqliteBackend::add` path auto-`INSERT OR IGNORE`s the session row so
callers don't need to upsert sessions explicitly.

**Auto-capture** for query-shaped commands (`sym`/`refs`/`callers`/`fuzzy`/
`prefix`) is opt-in via `CRABCC_AUTO_MEMORY=1`. Off by default, zero
overhead. Set the env var to have queries quietly accumulate as drawers.

Memory roadmap: M0 (trait + persistent backend, merged) → M0.5 (sqlite-vec
ANN) → M1 (fastembed-rs real embeddings + miner; LongMemEval R@5 ≥ 96.6%
gate) → M2 (BM25 hybrid) → M3 (full CLI/MCP surface) → M4 (KG ops). See
`docs/RESEARCH-mempalace.md` for the design.

## Where things live

- **Issue tracker:** GitHub issues at <https://github.com/peterlodri-sec/crabcc/issues>.
- **In-flight task log:** `.dev-tasks` (gitignored, local-only) tracks
  multi-agent work breakdown for current feature work.
- **Bench fixtures:** mc-mothership at `/Users/peter.lodri/workspace/mc-mothership`
  (NOT inside this repo; never copy or commit it). Pass via `--repo` arg or
  `REPO_FIXTURE` env var.
- **Skill + slash command:** `skill/crabcc/SKILL.md` and `commands/crabcc-init.md`.
  Symlink with `crabcc install-claude`.

## When unsure

Read `docs/ARCHITECTURE.md` first; it covers the data model and the indexing
pipeline at the level an editor agent needs. The `RESEARCH-*.md` documents
explain *why* — useful when a change feels like it should be obvious but
isn't. For memory-layer specifics, `docs/RESEARCH-mempalace.md` has the
full port plan + reuse map.
