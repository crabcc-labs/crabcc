# Changelog

All notable changes to crabcc are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
[SemVer](https://semver.org/).

## [Unreleased]

### Added — Test coverage for `memory forget` (follow-up to [#26](https://github.com/peterlodri-sec/crabcc/issues/26))
- PR #55 landed the `memory forget` CLI + `memory.forget` MCP tool
  but shipped no tests. This change closes that gap:
  - 4 Palace tests in `crates/crabcc-memory/src/palace.rs`: by-id
    removal, idempotency on missing id, before-in-wing scoping,
    empty-window noop. The before-in-wing test backdates rows via
    a direct `UPDATE drawers SET created_at = ?` so the cutoff is
    deterministic (no sleeping the test thread).
  - 3 CLI tests in `crates/crabcc-cli/src/memory.rs` for
    `parse_before_timestamp`: epoch seconds, RFC3339Z, garbage
    rejection (must surface as an error so we don't silently wipe
    everything by parsing to `0`).
  - 3 MCP dispatch tests in `crates/crabcc-mcp/src/lib.rs`:
    `forget --drawer ID` (incl. idempotent re-call), invalid arg
    combinations (no selector / both selectors / wing-without-before),
    and the RFC3339 cutoff path.

### Added — MCP `memory.search` ranked-output assertions (closes [#21](https://github.com/peterlodri-sec/crabcc/issues/21))
- The MCP `memory.search` tool already mirrors the CLI's hybrid /
  lexical / vector dispatch via `palace.search_with_mode` (#48).
  This change adds the missing test contract: every hit carries the
  full `DrawerHit` shape (`id`, `score`, `source_id`, `body`, `wing`),
  scores are monotonically non-increasing across all three modes, and
  unknown `mode` values surface as JSON-RPC errors instead of silently
  falling back to the default. Two new tests in
  `crates/crabcc-mcp/src/lib.rs`; existing memory smoke tests stay green.

### Added — Starship status-line surface (closes [#43](https://github.com/peterlodri-sec/crabcc/issues/43))
- `crabcc info --status-line` — terse one-liner suitable for
  Starship / tmux / VS Code status bars: `crabcc 87.2k · idx 12s ·
  mem 1.4k · 4 tools`. Position is the schema (tokens saved → index
  age → memory drawers → Claude Code tool calls), no qualifier text.
- `crabcc info --is-repo` — exit-only Starship gate. Returns 0 inside
  a crabcc-indexed repo (`.crabcc/index.db` reachable via walk-up from
  cwd), 1 otherwise. No stdout.
- `crabcc info --status-line --json` — same data as machine-readable
  JSON for editor plugins / VS Code statusline extensions.
- p95 ~10–20ms on M-series Mac after binary cache warm — fits inside
  Starship's 50ms render budget. Cold first-shot ~200ms (dyld map).
- Each segment degrades gracefully — a missing source drops that
  segment, never errors. Starship hides the whole module via
  `--is-repo` so "not in a crabcc repo" renders nothing.
- New `crates/crabcc-cli/src/status.rs` module with 12 unit tests
  (compact-number formatting, age formatting, CC project-path
  encoding, repo detection at root + walk-up, format-text dropping).

### Added — `docs/INTEGRATIONS.md`
- Worked Starship + tmux + VS Code configs side-by-side. Documents
  the four-segment shape, render-budget reasoning, and the JSON
  output schema.

### Added — `commands/crabcc-install.md` slash command
- Drop-in `/crabcc-install` for use inside a Claude Code session.
  Walks the user through the one-line `gh api …/install.sh | bash`
  install, the env knobs (`CRABCC_INSTALL_DIR`, `--no-completions`,
  `--no-claude`, `--check`, `--version=`), and a verification triple
  (`crabcc --version`, `crabcc info --status-line`, `crabcc go`).


### Added — `install.sh` upgrade-on-rerun (closes [#24](https://github.com/peterlodri-sec/crabcc/issues/24))
- Re-running `install.sh` is now a fast no-op when the local install is
  already current. The script probes for an existing `crabcc` at
  `$INSTALL_DIR/$BIN_NAME` (or anywhere on PATH), reads the local
  version via `crabcc --version`, then resolves the remote version
  with three fallbacks: pinned `--version=` arg → `gh release list -L 1`
  → `[workspace.package].version` parsed from `Cargo.toml` on the
  default branch.
- When `local == remote` the build step is skipped; completions and
  Claude symlinks are still refreshed (idempotent + cheap, useful when
  switching shells).
- New flags: `--force` (rebuild regardless), `--check` (report delta
  and exit; no writes).
- New Taskfile target `task install-upgrade-smoke` — runs install.sh
  three times (build → `--check` → no-op rerun) and asserts the no-op
  message appears on the second run. Output captured at
  `.summary/install-upgrade-smoke.txt`. Manual sweep target for the
  macOS arm64 + linux x86_64 deliverable; idempotent on no-op.


### Added — `simd-cosine` feature gate (issue #40)
- New `simd-cosine` cargo feature on `crabcc-memory` (default OFF;
  nightly-only). When on, the brute-force cosine helper at
  `backend/mod.rs` dispatches to a `Simd<f32, 8>`-chunked
  implementation; production 384-d MiniLM-L6-v2 embeddings hit the
  SIMD body 48 times with no tail.
- Two impls always present in the source tree: `cosine_scalar`
  (canonical, stable) and `cosine_simd` (gated). `cosine()` picks via
  `#[cfg(feature = "simd-cosine")]`.
- 4 new tests: `cosine_simd_matches_scalar_at_dim_384`,
  `cosine_simd_matches_scalar_with_tail` (covers `n ∈ {1, 7, 8, 9, 17,
  31, 33, 64, 65, 100, 384, 385}`), `cosine_simd_self_is_one`, and an
  always-on stable-side `cosine_falls_back_to_scalar_on_stable` that
  documents the default path.
- `#[ignore]`d perf smoke `cosine_perf_smoke` — runs scalar vs SIMD on
  a 384-d × 1000-row workload and prints the speedup. Invoke with:
  `cargo +nightly test --features simd-cosine -p crabcc-memory backend::tests::cosine_perf_smoke -- --ignored --nocapture`.
- Workspace `Cargo.toml` gains an explicit `rust-version = "1.86"` pin
  so toolchain drift gets caught by CI's MSRV row instead of a laptop.

### Added — `docs/RESEARCH-nightly-features.md`
- Triage of which nightly Rust features are worth adopting in crabcc
  and how to sandbox the toolchain risk. Covers `portable_simd` (verdict:
  adopt, behind `simd-cosine`), `iter_array_chunks` (skip — `chunks_exact`
  is stable and equivalent), `allocator_api` (defer until bumpalo proves
  insufficient), `try_blocks`, `gen` blocks, `box_into_inner`,
  `iter_intersperse`, `iter_collect_into`, `generic_const_exprs`.
- Crate-boundary stability stance: `crabcc-core`, `crabcc-mcp`,
  `crabcc-cli` strictly stable; `crabcc-memory` is the sandbox crate
  for nightly trials.
- Proposed CI matrix: stable (required) / nightly+simd
  (allowed-failure → required) / msrv 1.86 (required).

### Added — `docs/GRAPH.md` + `docs/RESEARCH-graph-prompt.md`
- New per-feature doc explaining the call-graph sidecar
  (`.crabcc/graph.json`): on-disk shape, build paths
  (`build_from_edges` vs `build_legacy`), internal consumers (`graph
  walk`/`cycles`/`orphans`/`crabcc go`), and the JSON-vs-SQL design
  trade-off.
- Companion research prompt: a drop-in template for further LLM
  research into where the sidecar should go next (storage layout,
  petgraph vs hand-rolled, incremental maintenance, edge-taxonomy
  expansion, scale limits of Tarjan SCC, recursive-CTE
  reconsiderations, visualization). Structured so the model can split
  work across sections.

### Added — `task coverage` + `scripts/coverage.sh`
- Workspace coverage report via `cargo-llvm-cov`, auto-installed on
  first run. `FORMAT=html` (default), `lcov`, `json`, or `text`. Output
  lands under `.summary/coverage/`.

### Added — `task doc` (rustdoc)
- Build the workspace rustdoc tree with `cargo doc --no-deps` and open
  `index.html` in the browser. Pass `OPEN=0` to skip the open, `DEPS=1`
  to include external-crate docs.

### Added — `task prep-pr` + `scripts/prep-pr.sh`
- Single-call pre-PR gate: fmt-check + clippy + test + doc-build (with
  `RUSTDOCFLAGS=-D warnings`). Output is teed to
  `.summary/prep-pr.txt` for paste-into-PR-body use. Exits non-zero on
  any failure.

### Added — richer crate-level rustdoc on `crabcc-core` and `crabcc-memory`
- `crabcc-core`'s `lib.rs` gained a full intro: per-repo state layout
  (`.crabcc/index.db`, `tantivy/`, `graph.json`, `fsst.symbols`), a
  modules-at-a-glance table, a `no_run` index-then-query example, and a
  cargo-features section.
- `crabcc-memory`'s `lib.rs` was expanded with a layers table, a
  `no_run` `Palace::open` + `remember` + `search` example, the search-mode
  matrix (hybrid/lexical/vector), the M0→M1b roadmap, and the cargo
  features list.

### Fixed — Taskfile YAML parse error
- The `smoke` target's bash heredoc (`cat > a.ts <<'EOF' …`) was
  inlined as a YAML plain scalar, which made the parser choke on
  `name: string`. Wrapped in a `|` literal block scalar — `task --list`
  now parses cleanly.

### Refreshed — Taskfile top-of-file comments
- "Quick start" and "Workflow extras" sections grouped by daily-driver
  vs. situational. New rows for `coverage`, `doc`, `prep-pr`,
  `local-ci`, `version`, `check-deps`, `doctor`, `aliases`,
  `docs-refresh`.

## [2.3.0] — 2026-04-30

### Added — modernized `install.sh` + one-line install
- One-line install: `gh api -H 'Accept: application/vnd.github.v3.raw'
  /repos/peterlodri-sec/crabcc/contents/install.sh | bash`. The script
  prompts for `gh auth login` if needed, clones via `gh`, builds with
  `cargo install --locked`, wires shell completions for the user's
  current shell (zsh/bash/fish), links the Claude Code skill + slash
  commands under `~/.claude/`, and prints a `crabcc go` next-step.
- Flags: `--no-completions`, `--no-claude`, `--version=`, `--bin-dir=`.
  Honours `CRABCC_INSTALL_DIR` and `CRABCC_REPO` env.
- README install section collapsed from a 3-step recipe to one line.

### Added — `crabcc go` one-shot init + Claude launch
- New zero-arg subcommand: `crabcc go`. In one breath it (a) detects whether
  the repo is initialized, (b) runs `full_index` (fresh) or `refresh`
  (warm), (c) rebuilds the Tantivy fuzzy/prefix sidecar, (d) rebuilds the
  call-graph sidecar, (e) opens or creates the per-repo memory palace at
  `.crabcc/memory.db`, (f) prints a stable status block (`✓ files / ✓
  symbols / ✓ edges / ✓ graph / ✓ drawers`), and (g) execs
  `claude --effort max --append-system-prompt <AGENTS.md> --no-chrome`
  so the LLM session starts pre-loaded with the crabcc primer.
- Falls back to a minimal hardcoded primer if `AGENTS.md` is absent.
- Friendly error path when `claude` is not on PATH — points at
  https://claude.ai/code and re-suggests `crabcc go`.
- 8 new unit tests covering init / idempotency / TS indexing / fallback
  prompt / `claude` discovery on empty PATH / report formatting.

### Added — `scripts/version.sh` + globalized `CRABCC_VERSION`
- Single source of truth for the workspace version. Parses
  `[workspace.package].version` from `Cargo.toml` once and exports
  `CRABCC_VERSION` to anything that sources it. `task version` (also
  `task version JSON=1`) prints from the same helper. The check-deps
  and doctor banners now display `crabcc vX.Y.Z` so log paste-ups carry
  provenance, and the Taskfile's top-level `vars:` exposes
  `{{.CRABCC_VERSION}}` for any future task.

### Added — `scripts/install-aliases.sh` + `task aliases`
- Idempotent installer for shell aliases that swap commonly-used legacy
  CLI tools for modern equivalents when the modern tool is on PATH:
  `grep→rg`, `find→fd`, `cat→bat`, `ls→eza`, `du→dust`, `df→duf`,
  `ps→procs`, `top→btop`, `tree→eza --tree`, `cd→zoxide`, plus crabcc
  shortcuts (`cc`, `cci`, `ccs`, `ccr`, `ccc`, `ccm`). Writes a fenced
  `# >>> crabcc-aliases >>>` block into `~/.zshrc` / `~/.bashrc` /
  `~/.config/fish/config.fish`; `MODE=remove` strips the block cleanly,
  `MODE=print` dry-runs.

### Added — M1a: hybrid memory search (issue #2)
- **FTS5 lexical index** for `drawers.body` (contentless `drawers_fts`
  virtual table keyed on drawer id) so KNN ids and BM25 ids share one
  namespace.
- **`Palace::search_hybrid`** issues both rankers and blends via
  Reciprocal Rank Fusion (k = 60). `Palace::search` now defaults to
  hybrid; ablation is exposed via
  `Palace::search_with_mode(SearchMode::{Hybrid,Lexical,Vector})`.
- **`crabcc memory search --mode {hybrid,lexical,vector}`** CLI flag and
  the matching `mode` arg on the `memory.search` MCP tool.
- **Backfill on open**: v2.1 databases (no FTS at write time) are detected
  and populated in one pass when `SqliteBackend::open` runs. Idempotent on
  subsequent reopens.
- 24 new unit tests across `palace.rs`, `backend/sqlite.rs`, and
  `backend/in_memory.rs` (RRF math, mode parsing, FTS round-trip,
  apostrophe / quote sanitisation, FTS backfill, FTS row drop on delete).
- *Deferred to M1b*: `FastEmbedder` (fastembed-rs / MiniLM-L6-v2) — gated
  behind a future `embed-fastembed` feature flag to keep the ONNX dep tree
  out of the default build.

### Added — `scripts/check-deps.sh` + `task check-deps`
- Portable doctor for external dev tools (cargo, jq, yq, rg, fd, gh,
  claude, repomix, …). Knows brew / apt / dnf / pacman / apk / zypper.
  Three modes: interactive (default), `--strict` for CI, `--json` for
  hooks. Header carries its own changelog block.

### Added — `scripts/doctor.sh` + `task doctor`
- Diagnostic for the crabcc toolchain itself: `crabcc` CLI on PATH,
  binary version vs. latest GitHub release, MCP server registration in
  `~/.claude.json`, slash-command + skill symlinks in `~/.claude/`,
  Taskfile hook health, smoke-test of `crabcc index` against a tempdir.
  Optional `--upgrade` runs `crabcc upgrade --apply`. Optional `--install`
  re-creates Claude Code MCP / commands / skill / hooks. Writes a full
  debug log to `.summary/doctor-YYYYMMDDHHMMSS.log` you can paste into a
  bug report.

### Added — `task docs-refresh`
- Spawns a detached `claude -p` session that rewrites README / AGENTS /
  CHANGELOG / CLAUDE / `commands/*.md` to match the current source tree.
  Output goes to `.summary/docs-refresh.log`. Idempotent.

### Added — `task local-ci`
- Standalone target that mirrors GitHub `ci.yml` (fmt-check + lint +
  test) and saves output to `.summary/local-ci.txt`. Used in PR
  descriptions when upstream CI is rate-limited.

## [2.2.2] — 2026-04-30

### Added — `sqlite-vec` ANN backend behind `memory-vec` feature ([#17](https://github.com/peterlodri-sec/crabcc/issues/17))

- **`memory-vec` cargo feature** on `crabcc-memory` (default OFF). When on,
  pulls in the bundled `sqlite-vec` C extension via the `sqlite-vec = "0.1"`
  Rust binding — links statically, no system-side install needed.
- **Auto-extension registration** — `SqliteBackend::open` calls
  `sqlite3_auto_extension(sqlite3_vec_init)` exactly once per process via
  `std::sync::Once`. Every subsequent rusqlite `Connection` inherits the
  extension transparently.
- **`drawers_vec` virtual table** — created at every `Backend::open` (gated
  `IF NOT EXISTS`). Schema: `drawer_id INTEGER PRIMARY KEY, embedding
  FLOAT[384]`. Dim matches MiniLM-L6-v2 (the M1 default in [#18](https://github.com/peterlodri-sec/crabcc/issues/18)).
  Empty until [#20](https://github.com/peterlodri-sec/crabcc/issues/20) wires the search path; M0 hash embeddings
  continue to live in `drawer_embeddings.bytes`.
- **+3 unit tests** in a new gated `vec_extension` test module — `vec_version()`
  round-trips, `drawers_vec` exists in `sqlite_master` after open, and the
  virtual-table creation is idempotent across three back-to-back opens.

## [2.2.1] — 2026-04-30

### Added — drawer_embeddings schema prep for M0.5 / M1 ([#19](https://github.com/peterlodri-sec/crabcc/issues/19))

- **`embedding_model TEXT NOT NULL DEFAULT 'hash-m0'`** column on
  `drawer_embeddings`. Tracks which embedder produced each row's vector so
  M0 (hash placeholder) and M1 (`fastembed-rs` MiniLM-L6-v2) embeddings can
  cohabit during model-upgrade migrations without losing old vectors.
- **`embedded_at INTEGER NOT NULL DEFAULT 0`** column on
  `drawer_embeddings`. Unix epoch when the vector was computed; `0` for
  rows migrated from a pre-2.5.3 db.
- **Idempotent ALTER ADD COLUMN** in `SqliteBackend::open` — same
  PRAGMA-introspect-then-`ALTER` pattern already used for `body_enc`.
  v2.0 / v2.1 / v2.2 `.crabcc/memory.db` files upgrade in place on first
  open; the migration is a no-op on already-migrated dbs.
- **+3 unit tests** in `crates/crabcc-memory/src/backend/sqlite.rs` —
  pre-existing v2.0-shaped db gains both columns; idempotent on repeat
  open; new inserts get the documented defaults.

## [2.2.0] — 2026-04-30

### Added — `crabcc info` + build labels embedded in the binary
- **`build.rs` in `crabcc-cli`** captures git provenance at compile time and
  emits five `cargo:rustc-env=` lines: `CRABCC_BUILD_COMMIT` (12-char sha),
  `CRABCC_BUILD_BRANCH`, `CRABCC_BUILD_TAG` (empty when HEAD isn't tagged),
  `CRABCC_BUILD_TIME` (UTC ISO-8601), `CRABCC_BUILD_TARGET` (Cargo's TARGET
  triple). Robust against shallow / detached / no-git checkouts: every git
  failure falls back to "unknown" or "" so the build never breaks.
  `cargo:rerun-if-changed=.git/HEAD,refs` triggers rebuild on commit-on-branch
  or branch-switch, so dev rebuilds always reflect the current sha.
- **`crabcc info` + `crabcc info --json`** — prints version, commit, branch,
  tag, build-time, target, plus a hand-curated one-line project summary
  (langs / MCP tools / token-shaping / speedup), suitable for status lines,
  bug reports, and paste-into-issue contexts.

## [2.1.0] — 2026-04-30

### Added — `crabcc upgrade` + shell completions
- **`crabcc upgrade`** (CLI + MCP tool + `/crabcc-upgrade` slash command) —
  checks GitHub for a newer release. Repo is private, so the implementation
  shells out to `gh` (which inherits the user's `gh auth login` credentials)
  rather than calling the public REST API. Three modes:
  - `--check` (read-only): print version delta + recommendations, exit.
  - default: same as `--check` but human-readable.
  - `--apply`: runs the check, then `rm`s `.crabcc/{index.db,tantivy/,graph.json}`
    so the next `crabcc index` rebuilds against the new binary's schema.
    The binary itself is the user's responsibility to update.
- Honors `$CRABCC_UPGRADE_REPO` for forks / mirrors.
- New module `crabcc_core::upgrade` with **12 unit tests** (semver compare,
  serde round-trip, cleanup_index idempotency).
- **`crabcc completions <shell>`** — emits a clap-generated completion script
  for zsh / bash / fish / elvish / powershell. Standard pattern:
  `crabcc completions zsh > "${fpath[1]}/_crabcc"`.
- New MCP `upgrade` tool with the same `{apply, repo}` surface.

### Docs
- README: install one-liner moved to the very top with a `gh auth login`
  prerequisite (private repo) + the zsh-completion install hint.

## [2.0.0] — 2026-04-30

**Edges-at-extract.** The `edges` table — sketched in v0.1, dormant in v1.x — is
now populated during `extract::walk` itself, one row per call site. Caller queries
become pure SQL; `crabcc graph build` drops from O(symbols × files) to a single
SELECT; new `graph cycles` and `graph orphans` queries fall out of the same data
for free.

Tracks issue [#3](https://github.com/peterlodri-sec/crabcc/issues/3). Co-shipped
with the FSST string-compression foundation already on main (v2.0.0-alpha,
issue #1) — together they form the v2.0.0 cut.

### Added
- **`extract::extract_edges`** — emits an `Edge` for every call expression
  encountered while descending a function/method body. Per-language node-kind
  matching: TS/TSX/JS `call_expression` (with `member_expression` receiver
  unwrap → property name); Ruby `call`; Rust `call_expression` with
  `field_expression` / `scoped_identifier` receivers; Go `call_expression`
  with `selector_expression`; Python `call` with `attribute` receivers.
  Co-located with symbol extraction via `extract_file_with_edges` to share
  the parser pass.
- **`Store::replace_edges(file_id, &[Edge])`** — mirrors `replace_symbols`.
  Plus `edge_count`, `callers_of`, `iter_call_edges`, `meta_get`, `meta_set`.
- **Pure-SQL caller path** — `query::callers_via_edges` and the gated
  fast-path in `query_callers`. One `SELECT … FROM edges WHERE dst_name = ?
  AND kind = 'call'` plus on-demand snippet fetch grouped by file.
  ~9ms on a fixture that previously took 1s+ via the per-file ast-grep walk.
- **`crabcc graph cycles`** — Tarjan SCC (iterative; deep call chains don't
  stack-overflow), filtered to size ≥ 2.
- **`crabcc graph orphans`** — defined symbols with no incoming callers
  (dead-code triage starting point).
- **`crabcc graph build` / `crabcc graph walk NAME`** — `graph` is now a
  parent subcommand. **Breaking** vs v1.x: `crabcc graph-build` →
  `crabcc graph build`; `crabcc graph NAME` → `crabcc graph walk NAME`.
- **MCP tools**: `graph_cycles`, `graph_orphans`. The existing `graph` tool
  is unchanged.
- **`IndexStats.edges`** field — full-index now reports symbol AND edge
  counts in the JSON summary.
- **Microbench**: `bench_graph_build_speedup` (gated `#[ignore]`) reports
  legacy vs SQL build wall-time on a synthetic 50-function fixture.
  Local result: **57× faster on 5 files / 50 fns** (54µs vs 3097µs).

### Changed
- **Schema v2**: `edges.src_symbol` is now TEXT (the enclosing symbol name)
  rather than INTEGER (FK to `symbols.id`). Mirrors `dst_name` and avoids a
  join on every caller query. New composite index `idx_edges_dst_kind` covers
  the hot SQL caller path. The migration in `Store::open` runs unconditionally:
  PRAGMA-introspects the column type and recreates the table only if the old
  shape is detected. v1.x indexes are upgraded losslessly (the table was
  always empty for them).
- **`CallGraph::build`** dispatches via the `edges_populated` meta flag:
  `build_from_edges` (single SQL scan) when '1', `build_legacy` (the
  pre-v2.0 walker, kept verbatim) otherwise. `crabcc index` sets the flag.
  `refresh` maintains it — partial v1→v2 upgrades correctly stay in legacy
  mode until the next full reindex.
- **CI**: PR runs scoped to crates touched by the diff; Ubuntu only; smoke
  E2E trimmed to the `index → sym → callers` hot path. Push-to-main keeps
  the full `--workspace` matrix as the backstop.

### Removed
- Top-level `crabcc graph-build` command (replaced by `crabcc graph build`).

### Internal
- **+22 unit tests** for edges (extract per-language, graph build/cycles/
  orphans, SQL caller parity, MCP tool dispatch) plus 1 perf microbench.

### Migration

If you have a v1.x `.crabcc/index.db`:

```bash
crabcc index   # rebuild — flips edges_populated='1', enables fast paths
```

Until you do, queries fall back to the v1.x ast-grep walker — correct,
just no faster than before.

## [1.1.0] — 2026-04-30

### Added — Language coverage (issue #4)
- **Rust** (`.rs`) — `function_item`, `struct_item`, `enum_item`, `trait_item`,
  `impl_item`, `mod_item`, `const_item`, `static_item`, `type_item`,
  `macro_definition`. `impl Foo { ... }` and `impl Trait for Foo { ... }`
  reattach inner methods with `parent="Foo"` (concrete type, generics stripped);
  fns inside impl blocks get retagged from `Function` to `Method`.
  Visibility: `pub` / `pub(crate)` / `pub(super)` / `pub(self)` preserved verbatim.
  `macro_rules!` → new `SymbolKind::Macro`.
- **Go** (`.go`) — `function_declaration`, `method_declaration`, `type_spec`,
  `const_spec`, `var_spec`. Method receivers (`func (r *Repo) Save()`) extract
  parent type with pointer + generics stripped (`*Repo[T]` → `Repo`).
  Visibility derived from name capitalization (Go's own export rule).
- **Python** (`.py`, `.pyi`) — `function_definition` (incl. `async def`),
  `class_definition`. `decorated_definition` (e.g. `@dataclass`) is unwrapped
  so the inner symbol carries the canonical name. Visibility: `_foo` and
  `__foo` are private; dunders (`__init__`, `__repr__`, …) remain public.
- `pattern.rs::lang_for` extended for Rust / Go / Python so `crabcc callers`
  resolves on all three. (Go `$RECV.X(...)` receiver-form calls match
  inconsistently across the Go grammar — bare-call form is reliable; tracked
  as cross-language pattern-coverage follow-up.)
- **+27 unit tests** (extract.rs +18, pattern.rs +9). Workspace total now
  **130 tests** (up from 103). All passing under `cargo nextest --profile ci`.

### Internal
- `SymbolKind::Macro` added (Rust). Round-trips through SQLite (`store.rs`
  `kind_str` / `kind_from_str`) and Tantivy (`fts.rs::kind_str`).

## [1.0.1] — 2026-04-30

Hotfix: drop `x86_64-apple-darwin` from the release matrix. The v1.0.0 release
workflow sat queued for 60+ minutes on the macOS-13 (Intel) runner pool, which
GitHub is in the process of deprecating. Intel-Mac users can `cargo install
--path crates/crabcc-cli` from source until we move to a self-hosted runner.
arm64 macOS, x86_64 Linux, and aarch64 Linux all still ship binaries.

### Docs
- `STORAGE_RESEARCH.md` → `docs/RESEARCH-storage.md` (alongside the other research docs).
- README: bench numbers reconciled with `bench/results/REPORT.md`
  (47–5500× vs grep, 5–68× vs rg, 206× aggregate, 414k tokens / batch).
- README status reflects v1.0.0 ship + 103 tests (86 core + 17 MCP).
- Removed broken `task-items/.tasks` link (file lives outside the repo);
  v2.0 milestone is the source of truth.

## [1.0.0] — 2026-04-30

First production-quality release. The features below are stable; their
storage formats (SQLite schema v1, Tantivy sidecar, graph.json, usage.log)
are upgrade-safe via additive migrations.

### Added
- **`crabcc watch [--debounce MS]`** — bulletproof FS watchdog sidecar. Worker
  thread (named `crabcc-watch`); debounced events (default 500ms) trigger
  incremental refresh; feedback-loop guard skips events under `.crabcc/`.
  4 unit tests + 1 ignored e2e.
- **`crabcc graph-build`** + **`crabcc graph NAME [--dir callers|callees] [--depth N]`** —
  call-graph sidecar persisted to `.crabcc/graph.json`. BFS expansion with
  cycle protection. 5 tests.
- **MCP `graph` tool** mirrors the CLI graph subcommand.
- **SVG logo** at `assets/logo.svg`.
- **`ARCHITECTURE.md`** — engineer-facing deep dive with mermaid diagrams.
- **`docs/RESEARCH-mempalace.md`** (1027 lines) — full Rust-port plan for the
  MemPalace AI-memory system as `crabcc memory` v2.0 subcommand. Vector-store
  comparison appendix (sqlite-vec chosen), implementation walkthrough, 12
  fine-tuning levers.
- **`docs/RESEARCH-fsst.md`** (272 lines) — FSST string-compression integration
  research for v2.0. Pessimistic gain ~30% storage reduction with <1ms p99
  per-row decode. Tracked in [issue #1](https://github.com/peterlodri-sec/crabcc/issues/1).
- **GitHub Actions test reporting** — `cargo nextest` with JUnit XML uploaded
  as build artifact (30-day retention, per matrix entry).
- **`crabcc files [--under PREFIX] [--lang LANG] [--ext EXT] [--limit N]`** —
  list indexed files. Replaces `ls -R` / `find -name` for code-file listings.
- Token-shaping flags on `refs` and `callers`:
  - `--limit N` — cap full hit list, early-stops the per-file walk.
  - `--files-only` — emit deduped JSON file list (~88% smaller than full hits).
  - `--count` — emit `{"count": N}` only (~99.98% smaller).
- MCP server tool schemas for `refs`/`callers` now expose `mode` and `limit`
  arguments matching the CLI flags.
- New `files` MCP tool.
- First-layer benchmark harness (`bench/raw-bench.py`) — CLI-vs-CLI bytes + ms
  comparison against `grep`/`find`/`cat` AND `ripgrep`/`fd`. No Claude session.
- Visualization (`bench/visualize.py`) emits PNG charts and `bench/results/REPORT.md`.
- Per-topic example docs in `examples/`: CLI overview + MCP wire protocol.
- `.devcontainer/devcontainer.json` for VS Code dev container.
- GitHub Actions: `ci.yml` (clippy, fmt, test, smoke), `release.yml` (multi-arch
  build with UPX compression for Linux/macOS-x86 binaries).

### Changed
- **`Store::open`** now sets `journal_mode=WAL`, `synchronous=NORMAL`,
  `foreign_keys=ON`, `mmap_size=30GB`, `temp_store=MEMORY`, `cache_size=16MB`,
  `busy_timeout=2s`, plus `PRAGMA optimize`. Compile-time assertion that
  `Store: Send`. New `analyze()` method.
- **Schema indexes**: `idx_symbols_file_line`, `idx_symbols_name_kind`,
  `idx_files_lang` for hot query paths.
- Snippet trim: `pattern.rs` and `refs.rs` cap line snippets at 80 chars
  (was 200 chars). ~60% smaller per-hit payload.
- Cargo release profile pushed to `lto = "fat"`, `panic = "abort"`,
  explicit `opt-level = 3`. ~5–10% runtime improvement, ~30s extra compile time.
- Added `[profile.dev-fast]` (`opt-level = 1`, minimal debug info) for fast iteration.
- Added `[profile.test]` `opt-level = 1` so tree-sitter-heavy tests aren't `-O0`.
- `query::find_refs` / `query::find_callers` retained as back-compat shims;
  new entry points `query_refs` / `query_callers` with `Mode` enum.
- SKILL.md rewritten: "tool ladder" section recommends `rg`/`fd`/`jq` as the
  fallbacks when crabcc isn't the right shape; deprecates plain `grep -rn` /
  `find -name` for repo work.

### Internal
- New types in `query.rs`: `Mode { Hits{limit}, FilesOnly{limit}, Count }` and
  `Output { Hits, Files, Count }` (untagged JSON for ergonomic output).
- Early-stop when `--limit` is reached avoids walking the rest of the file list.
- `--files-only` short-circuits per-file: dedupe-by-path, single insert per file.
- **+22 unit tests** across walker / store / outline / track / pattern / query / mcp / watch / graph
  (60 → 102 total; 2 ignored — both inherently FS-event-racy).
- **Removed**: `query::callers_via_edges` TODO stub. `pattern::smoke` is now
  `#[cfg(test)] pub(crate)` instead of a public API surface.
- **`cargo clippy --workspace --all-targets -- -D warnings`** clean.
- **`cargo fmt --all`** applied across the codebase.

### Notes
- Bench results (mc-mothership, ~13k indexed files): **47–5500× faster than
  `grep -rn`**, **5–68× faster than `ripgrep`** on whole-repo questions.
- Honest losses: single-file outline, small directory listings, regex-heavy
  callers-count where ripgrep's tight regex wins on raw speed (crabcc's edge
  there is structured output: kind/signature/parent metadata).

---

## [0.1.0] — 2026-04-29

Initial public-ish release. Highlights:

- Tree-sitter symbol extraction for TypeScript, TSX, JavaScript, Ruby.
- Per-language extractors in `extract.rs` produce
  `{name, kind, signature, parent, file, line_start, line_end, visibility}`.
- SQLite store at `.crabcc/index.db` with `files`, `symbols`, `edges` tables.
- Queries:
  - `sym <name>` — exact-match symbol lookup.
  - `refs <name>` — every identifier reference (tree-sitter walker).
  - `callers <name>` — call sites via ast-grep patterns
    `name($$$)` and `$RECV.name($$$)`.
  - `outline <file>` — every symbol in a file, ordered by line.
- Indexing:
  - `crabcc index` — full rebuild.
  - `crabcc refresh` — incremental, mtime + sha256 keyed (~250ms no-op on 13k files).
- Tantivy sidecar at `.crabcc/tantivy/`:
  - `crabcc fuzzy <query>` — Levenshtein distance 2.
  - `crabcc prefix <query>` — case-insensitive starts-with via `RegexQuery`.
- MCP server (`crabcc --mcp`) — JSON-RPC 2.0 over stdio.
  Tools: `sym`, `refs`, `callers`, `outline`, `index`, `refresh`, `fuzzy`, `prefix`.
- Token-savings tracker: `crabcc track` — heuristic estimate of tokens saved
  vs `grep + Read`, with session / 24h / all-time buckets.
- Skill (`skill/crabcc/SKILL.md`) and slash command (`commands/crabcc-init.md`)
  for Claude Code integration.
