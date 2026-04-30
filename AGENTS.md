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
| Bootstrap full dev env | `task setup` (uv + Ollama + model pull + stack up) |
| Run a crabcc agent | `crabcc agent --run "<task>" --backend ollama` |
| Launch Ollama stack | `task ollama-stack-up` (LiteLLM :4000 → Ollama) |

## Copilot cloud agent — MCP

crabcc exposes its symbol index, memory, and graph as MCP tools that GitHub
Copilot's cloud agent can call directly.

**Private repo note:** you can keep the repo private and still use Copilot cloud
agent MCP. The MCP server URL just needs to be reachable from GitHub's runners.
Cloudflared/Tailscale tunnels work fine; `CRABCC_AUTH_TOKEN` is encrypted in
the GitHub copilot environment.

```bash
# Expose crabcc serve over HTTPS (required by Copilot cloud agent)
crabcc serve &
cloudflared tunnel --url http://localhost:8090
# → Set CRABCC_MCP_URL=https://random.trycloudflare.com in copilot environment
```

Paste `.github/copilot/mcp.json` into **Settings → Environments → copilot → MCP configuration**.
Full setup guide: [`.github/copilot/README.md`](.github/copilot/README.md).

| MCP tool | Description |
|----------|-------------|
| `crabcc.sym` | Symbol definition lookup |
| `crabcc.refs` | Find all references |
| `crabcc.callers` | Find callers of a function |
| `crabcc.outline` | Outline a file |
| `crabcc.fuzzy` | Fuzzy symbol search |
| `crabcc.memory.search` | Search AI memory drawers |
| `crabcc.memory.remember` | Save a memory drawer |
| `crabcc.graph` | Call graph queries |

## Ollama agent backend

Default backend is `ollama` (since v2.8). Model: **qwen3.5:35b-a3b-coding-nvfp4**
(Apple Silicon MoE, 3B active/token, 256k context window).

```bash
# One-shot bootstrap (installs uv, Ollama, pulls model, starts stack)
task setup

# Run an agent
crabcc agent --run "trace callers of Store::open" --backend ollama

# Via Claude Code slash command
/crabcc-agent trace callers of Store::open

# Status
crabcc agent-ls --limit 5
task agent-runtime-smoke       # end-to-end smoke test
```

Stack topology (issue #105):
```
Claude Code / crabcc CLI
        ↓
free-claude-code (Anthropic-compat proxy)
        ↓
LiteLLM :4000  (prompt cache, SSE streaming)
        ↓
Caddy :11435   (Bearer-auth gate)
        ↓
Ollama         (qwen3.5:35b-a3b-coding-nvfp4)
```

ENV overrides (all optional):
- `OLLAMA_BASE_URL` — override backend URL (default `http://localhost:4000`)
- `OLLAMA_API_KEY` — LiteLLM master key (read from `~/.crabcc.local.api-key`)
- `CRABCC_OLLAMA_MODEL` — model override
- `OLLAMA_NUM_CTX` — context window (default 262144)

## iTerm2 HUD (issue #132)

Live status-bar showing active agent, token savings, and doctor health.

```bash
task install-iterm2     # copies daemon to AutoLaunch, prints activation steps
task iterm2-test        # run HUD unit tests (no live iTerm2 needed)
crabcc doctor iterm2    # verify daemon is running
```

See `apps/crabcc-iterm2/README.md` for the full guide (RPCs, control sequences,
key bindings, example use-cases).

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
installer/Crabcc.app/     # macOS .app bundle (issue #107) — menubar UI +
                          # bundled binaries + LaunchAgent templates.
                          # Built into dist/ via `task dmg`.
```

## macOS surface (issue #107)

The `Crabcc.app` bundle ships three things behind one drag-to-install:

- **Menubar UI** — single-file `installer/Crabcc.app/Contents/MacOS/menubar.swift`,
  compiled at DMG-build time with `swiftc`. Surfaces live process state,
  Taskfile entries as a clickable submenu, scheduled LaunchAgent tasks,
  and recent kill events. Emits JSON-lines telemetry at
  `~/Library/Logs/Crabcc/menubar.events.jsonl` parallel to the Rust
  crates' tracing-appender output.
- **Three LaunchAgents** registered by the installer's
  `Resources/scripts/install.sh`:
  `com.crabcc.menubar` (RunAtLoad + KeepAlive-on-crash),
  `com.crabcc.agentd` (5-min `crabcc refresh` tick, Background QoS),
  `com.crabcc.agent-guard` (every 20 min — `crabcc agent-guard` sweep).
- **Singleton agent-runs DB** at `~/.crabcc/_internal.db` (WAL).
  `crabcc agent` writes lifecycle rows; `crabcc agent-ls` /
  `agent-guard` / `agent-kills` read + maintain. Schema is additive
  (same rules as the symbol index — never `DROP COLUMN`).

Build: `task dmg` → `dist/crabcc-<version>.dmg`.
Bootstrap a fresh machine: `curl -fsSL …/scripts/bootstrap.sh | bash`.


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

**Bulk ingest (M2):** `crabcc memory mine project [PATH]` walks a
repository via `crabcc_core::walker::walk_repo` and stores one drawer
per text file under `wing="proj"`. `crabcc memory mine sessions [DIR]`
parses Claude Code JSONL transcripts (defaults to
`$HOME/.claude/projects/`) and stores one drawer per
`(user, assistant)` turn pair under `wing="session"`. Both are
idempotent — the existing `(source_id, sha256)` UNIQUE constraint
on `drawers` makes re-runs return the same id without inserting.

Memory roadmap status (issue #2): M0 (persistent backend) ✅ → M0.5
(`sqlite-vec` ANN, `--features memory-vec`) ✅ → M1a (FTS5 BM25 + RRF
hybrid) ✅ → M1b (`fastembed-rs`, `--features memory-embed`) ✅ → M2
(miners) ✅ → bench gate (`task memory-bench`, ≥ 96.6% R@5 on synthetic
fixture) ✅. Future M3-full (KG ops) tracked separately. See
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
