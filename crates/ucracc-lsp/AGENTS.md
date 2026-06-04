# AGENTS.md — `ucracc-lsp` (crabcc's internal LSP)

> Guidance for AI coding agents working on **`ucracc-lsp`** inside the
> crabcc monorepo. Repo-wide rules live in the root [`AGENTS.md`](../../AGENTS.md);
> this file only covers what's specific to this crate. Read both.

## What this crate is

`ucracc-lsp` is a **navigation + retrieval** language server backed by
`crabcc-core`'s symbol DB (SQLite), with native fuzzy/prefix search built
from it. It is **additive**: it
runs *alongside* the semantic server for each language (rust-analyzer,
pyright, gopls, …), which Zed/Neovim merge. It deliberately does **not** do
diagnostics, completion, formatting, or rename — those stay with the
semantic server.

What it *does* provide: `hover`, `definition`, `references`,
`documentSymbol`, repo-wide `workspace/symbol` (native prefix), call
hierarchy (from the `edges` table), and an AI-first `executeCommand`
surface (`ucracc.memory.search`, `ucracc.webfetch`, `ucracc.rerank` —
feature-gated).

Perf budget: cold start < 100 ms, hover/definition/references < 20 ms p95.
Don't regress these.

## Architecture (where things live)

| File | Responsibility |
|---|---|
| `src/server.rs` | `Backend` (the `LanguageServer` impl), state, `index_uri`. |
| `src/handlers.rs` | URL↔path helpers, result shaping for the LSP types. |
| `src/commands.rs` | `executeCommand` dispatch (feature-gated commands). |
| `src/incremental.rs` | `did_change` range-edit → in-memory text mutation. |
| `src/cache.rs` | `LruCache` (moka) — read-through query cache. |
| `src/lang.rs` | `Lang` enum + `SUPPORTED_LANGUAGE_IDS` (the advertised set). |
| `src/{markdown,yaml}.rs` | the two `handled_internally` extractors. |
| `src/{rerank,stats}.rs` | reranker + local usage/latency counters. |

`Backend` state (all shareable; `Backend: Send + Sync`):
- `open_docs: Arc<DashMap<Url, Arc<String>>>` — the in-memory text mirror.
- `store / fts / graph: Arc<std::sync::Mutex<Option<…>>>` — lazy-opened.
- `cache: Arc<LruCache>` (moka, lock-free reads).
- `root_config: tokio::sync::Mutex<Arc<RootConfig>>` — paths only.

## Invariants — do NOT break these

1. **Lazy open.** `initialize` records paths and returns in tens of µs; it
   must **not** open Store/Fts. `initialized` prefetches in the background;
   `ensure_store`/`ensure_fts` open on first real use. Keep `initialize`
   allocation-light.

2. **`index_uri` always does a FULL parse — never reuse a cached tree as an
   incremental hint.** tower-lsp dispatches notifications **concurrently**,
   so a racing `did_change` for the same URI can desync a cached tree's
   `InputEdit`s from the text being reparsed; feeding that to
   `Parser::parse` makes **tree-sitter panic**, which is fatal under the
   release `panic = "abort"` profile. There is intentionally **no**
   per-document `trees` cache. (Re-adding one reintroduces the race — see
   `tests/concurrency.rs`, which exists to catch exactly this.)

3. **Indexing must never panic.** A panic inside the `index_uri`
   `spawn_blocking` poisons the shared `store` mutex and wedges *all*
   further indexing. Extractors slice defensively (`src.get(a..b)?`, not
   `&src[a..b]`). Prefer returning `None`/`Err` over `unwrap`/index panics
   on any tree- or input-derived offset.

4. **Per-worktree binary/path resolution.** Anything host-specific
   (`worktree.which`, env) is resolved **per worktree**, never cached
   across worktrees — a local project and a remote SSH project in one
   session have different hosts.

5. **`indexPath` is a location override only.** `initialization_options.
   indexPath` moves *where* the index lives, not the root it was built
   against; stored paths stay relative to `repo_root`. Don't "fix" the
   path-resolution to chase a different root's index.

6. **Schema is additive.** Never `DROP COLUMN`. See root AGENTS.md.

7. **Keep `src/lang.rs` ⇄ `editors/zed/crabcc/extension.toml` in sync.**
   If you add a language to `SUPPORTED_LANGUAGE_IDS`, add the matching Zed
   `language_servers.ucracc-lsp.{languages,language_ids}` entry.

## Building & testing

The workspace requires a recent stable (CI is on **Rust 1.96**); a much
older local stable may not build the dep graph.

```bash
cargo test -p ucracc-lsp                    # unit + integration + concurrency
cargo test -p ucracc-lsp --test concurrency # the race/stress suite
cargo clippy -p ucracc-lsp --all-targets -- -D warnings
cargo bench -p ucracc-lsp --bench fixture_scale -- --test   # scaling smoke
task lsp-race                               # concurrency suite under ThreadSanitizer (nightly, opt-in)
```

- `tests/concurrency.rs` hammers one `Backend` from many parallel Tokio
  tasks; it asserts no panic, no deadlock (timeout-bounded), last-write-wins
  consistency. It shares the backend through a small `unsafe Send+Sync`
  wrapper guarded by a compile-time `Backend: Send+Sync` assertion — if you
  change `Backend`'s fields, keep that assertion valid.
- `benches/{baseline_vs_lsp,extractor_cost,fixture_scale}.rs` — criterion;
  `fixture_scale` is parameterized by symbol count to catch superlinear
  regressions.

**Known flake (not yours):** under heavy parallel test execution the setup
`full_index` can trip `UNIQUE constraint failed: edges...` from a duplicate
edge in `crabcc-core`'s `replace_edges` (a plain `INSERT`). It's a
`crabcc-core` robustness gap (fix: `INSERT OR IGNORE`), not an `ucracc-lsp`
bug — don't "fix" it by weakening the concurrency tests.

## Versioning

`ucracc-lsp` is versioned **independently** of the workspace (its own
`ucracc-lsp-vX.Y.Z` tag + `release-ucracc-lsp.yml`). Bump
`crates/ucracc-lsp/Cargo.toml` `version` in lockstep with the tag, not with
the workspace `5.x` line.

## The Zed extension

`editors/zed/crabcc` is the Zed shim (WASM, `wasm32-wasip1`, excluded from
the workspace). It only *launches* `ucracc-lsp` and forwards settings — no
core logic. Its published, GPLv3 source lives in a separate public repo
(crabcc is private); keep the two in sync when you touch the extension.
