# Changelog

All notable changes to crabcc are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
[SemVer](https://semver.org/).

## [Unreleased]

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
