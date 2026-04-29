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

## Quick orientation

| You want to… | Do this |
|---|---|
| Find a definition | `crabcc sym <Name>` |
| Find references / call sites | `crabcc refs <Name>` / `crabcc callers <Name>` |
| Outline a file | `crabcc outline path/to/file.rb` |
| Build & test | `task` (= `cargo build --release && cargo test --workspace`) |
| Lint gate | `task lint` (= `cargo clippy --workspace --all-targets -- -D warnings`) |
| Format | `task fmt` |
| Local CI dry-run | `task ci` |
| Run the FSST bench | `task bench-compress REPO_FIXTURE=/path/to/big-repo` |
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
│       ├── extract.rs    # tree-sitter symbol extractors (TS/JS/RB)
│       └── …
├── crabcc-cli/           # Binary: `crabcc` (clap) + `crabcc --mcp` shim
└── crabcc-mcp/           # Library: stdio JSON-RPC 2.0 MCP server logic

bench/                    # raw-bench.py (vs grep/find), compress-bench.py (FSST gate)
docs/
├── ARCHITECTURE.md       # Read this before touching cross-crate code.
├── RESEARCH-fsst.md      # FSST integration design + release-gate criteria.
└── RESEARCH-mempalace.md # Memory-palace storage research (deferred work).

schema/001_init.sql       # Source of truth for the DB schema. ALL changes are additive.
install/                  # crabcc install-claude templates (hooks-claude.json)
```

## Conventions agents should respect

- **Don't break the gate.** `crates/crabcc-core/src/compress.rs` and
  `crates/crabcc-core/src/store.rs` are load-bearing for the v2.0.0-alpha
  release. Run `task bench-compress` after changes to either.
- **Schema is additive.** Never `ALTER TABLE … DROP COLUMN`. The pattern is
  add column + idempotent `ALTER` in `Store::open` (see how `signature_enc`
  was landed).
- **Tests are 92 with feature on, 86 without.** Both must pass.
- **Don't mass-rewrite imports / spacing on files you barely touched.** The
  linter and `task fmt` keep things consistent — let them do their job.
- **One feature, one PR.** Don't fold release prep, refactors, and a feature
  into a single commit.

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
isn't.
