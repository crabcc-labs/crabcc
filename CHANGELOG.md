# Changelog

All notable changes to crabcc are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
[SemVer](https://semver.org/).

## [Unreleased]

## [5.0.0] — 2026-06-01 — *stable baseline*

Promotes the battle-tested v4.5 sharpening line to a stable **v5.0** major, so
downstream systems (FieldFeed, agent-os-v2, crabcc-private) can pin a stable v5
baseline. **No behaviour changes vs 4.5.0** — this is the version-stability cut
the v5.x line builds on. The v5 *feature* scope (streaming queries, waste
analyzer, godfather `report --json`) lands in **5.1** (see #616).

## [4.5.0] — 2026-05-29 — *the sharpening release*

v4.5 is the discipline release. Two-thirds of the diff is *removal* — six
surfaces and four agent integrations cut to restate the moat: **crabcc is
the deterministic substrate between tree-sitter and the agent.** Every
feature that didn't extend that statement got shown the door.

The one feature *added* — cross-repo `--workspace` queries — is the moat
extension itself: symbolic lookup that now works across every indexed repo
in `$CRABCC_HOME/repos/*/`, not just the one you're standing in.

### Added

- **Cross-repo symbolic lookup via `--workspace`** (the anchor feature). Reads
  against every indexed repo under `$CRABCC_HOME/repos/*/` discovered by
  filesystem walk (no manifest, no write coordination, microseconds at the
  ~60-repo scale). Supported in v4.5: `sym`, `fuzzy`, `prefix`. Output is a
  stable envelope:

  ```json
  {
    "workspace": true,
    "queried_repos": N,
    "total_hits": M,
    "by_repo": [{"repo": "<key>", "count": K, "hits": [...]}, ...]
  }
  ```

  `refs`, `callers`, and `graph walk` defer to v5 — they need per-repo
  source-dir resolution which is its own design decision. The error message
  points the user at the per-repo equivalent.

  Mutually exclusive with `--root`. Not yet wired through the MCP surface
  (single-store-bound in v4.5).

- **pi agent integration** (the second supported integration alongside
  Claude Code). pi reads skills from `~/.pi/agent/skills/<name>/SKILL.md`
  (global) and `.pi/skills/<name>/SKILL.md` (project) and enables them via
  the `skills` array in `settings.json`. The installer symlinks
  `skill/crabcc/SKILL.md` into pi's skill dir and prints the settings
  fragment. pi does not currently support MCP servers natively; when it
  does, this integration migrates.

### Removed (the sharpening cuts)

**Surfaces:**

- `apps/crabcc-hitl-agent/` — HITL agent (Telegram was its only frontend)
- `apps/crabcc-notify-ext-poc/` — POC that didn't graduate
- `apps/jobs-worker/` — background job runner, off-moat
- `apps/crabcc-iterm2/` — iTerm2 HUD
- `apps/crabcc-chrome-extension/` + `crates/crabcc-chrome/` — Chrome
  extension + native-messaging Rust crate. Browser-side agent work belongs
  to a separate lane.
- `taskfiles/telegram.yml`, `taskfiles/hitl.yml`, `scripts/telegram-*.sh` —
  Telegram tooling

**Agent integrations** (kept: Claude Code + pi):

- Cursor (`install/integrations/hooks-cursor.json` + Rust support + docs)
- Gemini CLI (`install/integrations/gemini-settings.fragment.json` + Rust + docs)
- OpenCode (`install/integrations/opencode.fragment.jsonc` + Rust + docs)
- LangChain / LangGraph / LangSmith (`install/integrations/langchain/` + Rust + docs)

**CI:**

- `.github/workflows/linear-sync.yml` — Linear sync, Linear integration removed
- `tools/linear/` — Linear backfill scripts
- Dead secrets from `load-copilot-env.yml`: `LINEAR_API_KEY`,
  `TELEGRAM_BOT_TOKEN`, `TELEGRAM_CHAT_ID`, `CURSOR_ADMIN_KEY`

### Architectural decisions worth surfacing

- **Cross-repo schema: union materialized view (filesystem-walked) for v4.5.**
  Virtual-table-with-lazy-ATTACH is the right end-state but caps at 10
  databases by default in SQLite, and benchmarking it against the current
  ~60-repo scale isn't justified until users have more repos. Targeted
  refactor in 2-3 minor versions after v4.5 ships.
- **No MCP cross-repo in v4.5.** The agent surface stays single-store-bound
  for now. Cross-repo via MCP needs schema decisions about how to express
  "this hit came from repo X" in a way agents can act on — a v5 design
  conversation.
- **Confident migration tone.** This release isn't an apology for cutting
  features; it's a statement about what crabcc is *for*. The above list of
  removals is the deliverable, not collateral damage.

## [4.1.0] — 2026-05-21

Forge visualizer improvements, developer tooling hardening, and local CI via
`act`. All crabcc-viz changes are in the standalone crate (excluded from the
workspace); the workspace version bumps in lock-step for release tracking.

### Added

- **`nektos/act` integration.** `.actrc` maps the `[self-hosted, linux,
  hetzner]` runner label to `catthehacker/ubuntu:act-22.04`; `--reuse` keeps
  the container warm for incremental re-runs. Two new `Taskfile` targets:
  - `task act` — runs the full push-event CI workflow locally.
  - `task act-pr` — generates a PR-event JSON from `git merge-base` and
    scopes the test run to the diff, matching real CI behavior.
- **Conventional Commits enforcement** via a new `scripts/git-hooks/commit-msg`
  hook. Pattern: `type(scope)?: description`. Valid types: `feat`, `fix`,
  `chore`, `refactor`, `docs`, `test`, `ci`, `perf`, `style`, `build`,
  `revert`. Merge/revert/fixup/squash commits are exempt. Bypass:
  `CRABCC_SKIP_HOOKS=1` or `--no-verify`.
- **Scoped pre-push test gate** via a new `scripts/git-hooks/pre-push` hook.
  Runs `cargo nextest` (falling back to `cargo test`) for every workspace crate
  touched by the pushed commits — same diff scope as the CI `test` job on
  `pull_request` events.
- **`scripts/install-hooks.sh` generalized.** Now installs every executable in
  `scripts/git-hooks/` (pre-commit, commit-msg, pre-push) via symlinks from a
  single loop, rather than a hardcoded list. `--remove` and `--print` work for
  all hooks.
- **Pre-commit TypeScript typecheck** (`bun run typecheck`) added for staged
  `crates/crabcc-viz/web/src/**/*.{ts,tsx}` and `package.json` changes.
- **Pre-commit `actionlint`** for staged `.github/workflows/*.yml` files.
- **Pre-commit `yq` YAML validation** for `openapi.yaml`.
- **`deny-draft-prs.yml` workflow.** Marks all PRs as ready for review on
  `opened`, `reopened`, `converted_to_draft` events. Enforces the repo policy
  that all PRs land in "ready for review" state.
- **forge: HTTP status propagation.** New `ForgeHttpError` type carries the
  raw HTTP status from GitHub API errors through `anyhow` to the route handler,
  so 401/403/404 errors from the forge routes return the correct HTTP response
  code instead of always 400.
- **forge: rate-limit awareness.** `X-RateLimit-Remaining` / `X-RateLimit-Reset`
  headers are absorbed from every GitHub API response into a module-level
  `Mutex<RateLimit>` cache. `ForgeConfig` exposes `rate_limit_remaining` and
  `rate_limit_reset` fields.
- **forge: paginated file list.** `get_pr_files` pages up to 3 pages
  (300 files max) and returns a `truncated: bool` flag when the cap is hit.
  `PrImpactGraph` carries the flag to the frontend.
- **forge: edge-dedup via `HashSet`.** Replaced O(n²) `iter().any()` scan
  in impact graph construction with a `HashSet<(String, String)>` accumulator.
- **`DiffViewer` memoization.** `parsePatch` result computed via `useMemo`
  to avoid re-parsing on every re-render.
- **`ImpactGraph` D3 loading state.** Spinner overlay shown until the lazy
  `Promise.all([import d3, import d3-dag])` resolves.
- **Truncation warnings in `PrDetail`.** Diff tab warns when `changed_files >
  files.length`; impact tab warns when `impactData.truncated` is set.

### Fixed

- **`compute_hotspots` return type.** Was returning `total_commits.min(total_files)`
  for `total_commits_scanned` — semantically wrong. Now returns
  `(hotspots, total_commits, total_files)` as a 3-tuple; `analytics_snapshot`
  destructures correctly.
- **`get_pr_files` call-site destructuring.** Route handler now destructures
  `(files, _truncated)` after the return-type change.

### Chores

- `.gitignore`: added `node_modules/`, `.idea/`, `.vscode/`
  (`!.vscode/settings.json` preserved), `*.swp`, `*.swo`, `.secrets`.
- `.dockerignore`: added `apps/*/node_modules/` and `apps/*/bun.lock`.

### Fixed

- **`Store::replace_edges` / `callers_of` / `refs_of`** rewritten against the
  v4 `(src_symbol_id, dst_symbol_id, kind, line)` columns. The previous code
  still queried the dropped v3 columns; 40 of 282 `cargo test` cases panicked
  with `no such column` at runtime. (#550)
- **`Store::replace_symbols` now persists `parent_id`.** Previously the bulk
  path hardcoded `None::<i64>`, dropping every impl/class linkage on the
  ingest path. (#550)
- **`iter_all_symbols` / `symbols_in_file` / `find_by_name`** join through
  `symbols.parent_id` so `Symbol.parent` is no longer always `None`. (#550)

### Tests

- New `crates/crabcc-core/tests/v4_regression.rs` and
  `crates/crabcc-core/tests/v4_cross_functional.rs`: end-to-end coverage of
  `full_index` → `Store` → `query::*` and the four KG ops (`why`,
  `blast_radius`, `hot_symbols`, `importers`). Polyglot (Rust + Python +
  TypeScript) indexing, `refresh_delta` round-trip, and FK-cascade contract
  also covered.
- `crates/crabcc-cli/tests/integration/graph_v4.rs` is now wired into the
  test binary (was previously orphaned).
- KG-on-real-ids tests (`why`, `blast_radius`, `hot_symbols`) and the CLI
  `graph why|blast-radius|hot-symbols` tests are `#[ignore]`'d pending
  CRIT-5 resolver wiring + CLI surface, both tracked for v4.0.1.

### Chores

- Two unused imports cleared (`extract::mod::NameOnlyResolver`,
  `query::importers::VecDeque`) so `clippy -D warnings` stays green.

## [4.0.0] — 2026-05-16

Data Layer 2.0. Breaking schema change: edges are now keyed by
`symbol_id` foreign-keys instead of `dst_name TEXT`, restoring resolution
for `Foo::open` vs `Bar::open` and enabling real knowledge-graph traversal.
Three first-class symbol resolvers (Rust, JS/TS, Python). Four new graph
ops (`blast-radius`, `why`, `hot-symbols`, `importers`). Pre-v4 indexes
are auto-wiped and rebuilt on first open via the v3.2 `needs_reindex`
plumbing — no flag, no migrator, no user choice.

### Breaking
- **Schema v4.** The `edges` table is rebuilt with `(src_symbol_id,
  dst_symbol_id)` FK columns; the pre-v4 `(src_file_id, src_symbol TEXT,
  dst_name TEXT)` shape is dropped. `symbols` gains `qualified TEXT` and
  `parent_id INTEGER REFERENCES symbols(id)` (replacing the loose
  `parent TEXT`). A new `unresolved_names` sentinel table backs name-only
  recall for languages without a resolver yet (Ruby, Java, Swift).
- **`CallGraph` public API.** `outgoing`, `incoming`, `cycles`, `orphans`
  now take and return `i64` symbol-IDs instead of `String` symbol names.
  Callers that need human-readable output resolve IDs back to qualified
  names via `Store::find_by_name` at render time. The v1.0.0 `build_legacy`
  walker is removed — v4 indexes always populate the `edges` table.

### Added
- **`crabcc graph blast-radius <symbol> [--depth N] [--kind …]`** —
  reverse transitive closure: everything that transitively depends on the
  given symbol.
- **`crabcc graph why <src> <dst> [--max-depth N]`** — shortest
  call-graph path between two symbols (bidirectional BFS).
- **`crabcc graph hot-symbols [--top N] [--kind …]`** — symbols ranked
  by in-degree (most-called first).
- **`crabcc graph importers <path> [--depth N]`** — file-level edge
  rollup: which files transitively reference the given path.
- The same four ops are exposed as MCP tools: `graph.blast_radius`,
  `graph.why`, `graph.hot_symbols`, `graph.importers`.
- **Resolvers.** First-class scope walkers for Rust (use_, mod, impl),
  TypeScript / JavaScript (ES imports, class scope), and Python (imports,
  class scope) in `crabcc-core::extract::{resolve_rust, resolve_ts,
  resolve_python}`. The extractor is now two-pass (pass-1 collects defs,
  pass-2 routes uses through a `Resolver` trait).

### Changed
- **`crabcc graph walk/cycles/orphans`** keep their flag shape but their
  output now references symbol-IDs (and qualified names) rather than raw
  destination strings — collisions like `Foo::open` vs `Bar::open` are no
  longer collapsed.
- Indexes built before v4.0.0 are auto-wiped and rebuilt on first open.
  Stale-index detection moves from the v3.2 `ref_edges_built` flag to a
  new `schema_v4_built` flag. Users see a `crabcc: index built with
  schema v3; wiping and re-indexing for symbol-ID edges...` message
  identical in shape to the v3.2 message they already saw on first
  upgrade. Full re-index of this 13k-file repo completes in <60 s on an
  M-series Mac.

## [3.2.0] — 2026-05-16

Bug fixes for `lookup refs` and `lookup callers` LSP/CLI commands.

### Fixed
- `lookup refs <struct>` no longer returns `[]` for Rust structs, enums, and
  type aliases. The Rust extractor now emits `kind=ref` edges for every
  `type_identifier` node in non-definition position (return types, parameter
  types, let bindings, impl headers, struct construction, generic arguments).
- `lookup callers <fn>` with qualified names (e.g. `crate::module::Fn`)
  now matches correctly. A `bare_name()` helper strips path qualifiers before
  the edge lookup. `query_refs` falls back to `query_callers` for languages
  without dedicated ref-extraction support.
- Indexes built before v3.2.0 are automatically detected on open via
  `schema_version` (bumped from 2 to 3). A stale index is wiped and rebuilt
  transparently before serving the first command, including an FTS sidecar
  rebuild.

## [3.1.0] — 2026-05-16

Pre-release cleanup cycle. Pure refactor — no behaviour change,
no public-API break. Three large single files modularized, one
panic-on-bad-path replaced with proper error propagation, MSRV
and broken doc references brought back in sync, and one round of
patch-level dependency bumps.

### Added
- Per-crate `docs/HOW_IT_WORKS.md` now linked from `README.md`,
  `AGENTS.md`, `CONTRIBUTING.md`, and `DESIGN.md` as the
  canonical deep-dive entry points.

### Changed
- **`crates/crabcc-memory/src/palace.rs`** (1778 LoC) split into
  `palace/{mod,search_mode,path,registry,rrf}.rs`. Public API
  preserved via `pub use` re-exports; external imports through
  `crabcc_memory::palace::{Palace, PalaceRegistry, SearchMode,
  find_git_root, resolve_db_path, DEFAULT_PALACE_CACHE_CAPACITY,
  GIT_ROOT_CACHE_TTL, PALACE_CACHE_TTI}` keep working unchanged.
- **`crates/crabcc-memory/src/backend/sqlite.rs`** (1696 LoC) split
  into `sqlite/{mod,encoding,ensure}.rs`. The `Backend` impl stays
  intact in `mod.rs`; only the pure-function helpers move out.
- **`crates/crabcc-mcp/src/lib.rs`** (2430 LoC) split into
  `transport.rs`, `schema.rs`, `dispatch.rs`. Public exports
  (`serve_*`, `handle*`, `tools_def*`, `OPENAPI_YAML`,
  `dev_mode_from_env`) preserved; `memory.rs`'s
  `use crate::{arg_str, str_field, tool_schema}` import preserved
  via a `pub(crate) use schema::*` re-export.
- **`crates/crabcc-viz/src/lib.rs`** (3562 LoC) split — extracted
  `banner.rs` (179 LoC), `query.rs` (85 LoC), `graph.rs` (196 LoC),
  `bootstrap.rs` (129 LoC), `memory_view.rs` (99 LoC). lib.rs now
  2905 LoC, mostly the agent surface + tests.
- **Cargo.toml** — `rust-version` corrected from `1.87` → `1.86`
  to match `clippy.toml` (the `fsst-rs 0.5.10` floor is the actual
  MSRV bottleneck). README badge + Dockerfile already reflected
  1.86.

### Fixed
- **`crates/crabcc-cli/src/main.rs`** — `db.parent().unwrap()`
  during `create_dir_all` now propagates via `anyhow::Context`
  instead of panicking on pathological db paths.

### Removed
- **`docs/{ARCHITECTURE,ROADMAP-v2.5,RESEARCH-{fsst,mempalace,
  storage}}.md`** dead links scrubbed from `README.md`,
  `AGENTS.md`, `CONTRIBUTING.md`, `DESIGN.md`, and
  `apps/crabcc-notify-ext-poc/README.md`. Replaced with links
  to the live `crates/crabcc-core/docs/HOW_IT_WORKS.md` /
  `docs/RUST-ANTHOLOGY.md` / `docs/desktop/ARCHITECTURE.md` files
  that are actually on disk. (The original `docs/ARCHITECTURE.md`
  was intentionally untracked in #53.)
- **`.summary/v1.6-summary.xml`** stale release-summary artifact
  (regeneratable via `task gen-summary`).

### Deprecated (will be removed in 3.2.0)
- The hidden flat / pre-grouping CLI aliases — `crabcc {sym, refs,
  callers, outline, files, grep, fuzzy, prefix, refresh, watch,
  fts-rebuild, track, install-claude, upgrade, completions, openapi,
  compress, agent-run, agent-ls, agent-guard, agent-kills,
  ollama-stack, model-info, debug-service-discovery}` — were
  superseded by the `crabcc <group> <op>` form in #177 (e.g.
  `crabcc lookup sym`, `crabcc setup install-claude`, `crabcc agent
  run`, `crabcc info services`). They continue to work in 3.1.0
  with a deprecation warning; a follow-up PR will remove the
  variants + dispatchers entirely.

### Dependencies
- `cargo update` — 44 transitive patch/minor bumps:
  - `tokio` 1.52.1 → 1.52.3
  - `tonic` + `tonic-prost` 0.14.5 → 0.14.6
  - `tower-http` 0.6.8 → 0.6.10
  - `pin-project` 1.1.11 → 1.1.13
  - `openssl` 0.10.78 → 0.10.79 (security)
  - `serde_with` 3.19.0 → 3.20.0
  - `winnow` 1.0.2 → 1.0.3, `siphasher` 1.0.2 → 1.0.3
  - `tree-sitter-{dart,scala,swift,kotlin}` grammars
  - `wasm-bindgen` 0.2.120 → 0.2.121
  - Removed transitives: `plain v0.2.3`, `redox_syscall v0.7.4`

No workspace dependency pins (Cargo.toml `[workspace.dependencies]`)
changed; major bumps (`clap 5`, `ast-grep 0.43`, `fastembed 6`,
`moka 0.13`, Edition 2024) deliberately deferred.

## [3.0.0-rc.3] — 2026-05-04

Desktop theme + layout alignment release. The GPUI binary now
reads as the same product as the web dashboard at `/live` —
core palette mirrored token-for-token, tile typography aligned,
five named theme presets with live switching + persistence, and
a proper settings panel with an about modal. 13 commits since
v3.0.0-rc.2.

### Added — desktop theme system
- **5 named palettes** wired through a single `Palette` struct
  + `Palette::ALL_NAMES` registry: `web_dark`, `web_light`,
  `cyberpunk_neon`, `mono`, `high_contrast` (#356, #357, #359).
- **`CRABCC_DESKTOP_PALETTE` env var** for explicit override at
  process start (#357).
- **Live theme switcher** — `◐ <palette_name>` button in the
  header cycles through palettes on click; `window.refresh()`
  forces every observed entity to repaint without restart
  (#361).
- **Persistence** to `~/.config/crabcc-desktop/palette` —
  user's selection survives restart. Preference order:
  env var > persisted file > default (#362).
- **`Palette` as gpui `Global`** — per-route widgets read
  cyberpunk accents directly via `cx.global::<Palette>()`
  without re-deriving (#368).
- **First cyberpunk-accent applications**: running-agent dots
  on the Home tile pick up `cyber_cyan` (#368); services
  reachable / down indicators use `cyber_cyan` / `cyber_amber`
  (#369). Palette-aware — flips correctly with each preset.

### Added — desktop layout alignment
- **Tile + KPI typography** — uppercase + muted-fg titles +
  optional metadata pill on the right (mirrors the web's
  `.dash-tile-title` rule) (#363).
- **Memory drawers preview tile** on the Home dashboard —
  three states (loading / not-bootstrapped / empty / 5-row
  preview) mirroring the web `<DashTile title="memory
  drawers">` (#365). New `fmt_age_short` helper matches the
  web's `fmtAge` selector exactly.

### Added — desktop settings + about
- **Inline `SettingsPanel`** opened via the header `⚙` gear
  button (#366). Three sections shipped:
  - **THEME** — palette picker (jumps directly to a chosen
    palette, persists, auto-closes the panel).
  - **ALERTS** — `mute alerts` + `echo to Notification
    Center` toggle rows mirroring the header buttons (#372).
  - **About link** — opens the about modal.
- **`AboutModal`** overlay surface (#372) — version + repo +
  `CARGO_PKG_DESCRIPTION` blurb + curated `BUILT WITH`
  dependency rollup (gpui ecosystem, HTTP / serialisation,
  native macOS bindings, etc.). Backdrop click closes.

### Notes for the RC

- The cyberpunk-skinned web panels (Ollama key, Services,
  Agents) still use legacy CSS — palette-strategy decision
  pending before they migrate.
- Per-route cyberpunk treatments on the desktop are first
  applications only — Knowledge wing colours, kill button,
  KPI card left-borders are easy follow-ups via the same
  `cx.global::<Palette>()` pattern.

## [3.0.0-rc.2] — 2026-05-04

Follow-on RC. Pulls in the Track B (web `/live` dashboard shadcn
refresh) groundwork that landed after rc.1, plus a small test-
infra fix. No desktop / CLI / MCP code changes — the desktop
crate binary is identical to rc.1.

### Added — Track B (web dashboard shadcn refresh)
- **Tailwind v4 foundation** wired into `crates/crabcc-viz/web`
  with the `d3-force` dep regression fixed (#344).
- `cn()` className-merge helper landed; the `Header.tsx`
  component is the first visible surface ported to Tailwind
  utility classes (#345).
- shadcn-style **`Button` drop-in component** + Header icon-row
  using it (#346). First reusable shadcn primitive in the
  `crabcc-viz/web` tree; future component ports (Card, Dialog,
  Input) build on the same registry.

### Fixed
- `crabcc-viz/web` test suite was failing on `document` not
  defined; registered `@happy-dom/global-registrator` so vitest
  has a DOM under each test (#352).

### Notes
4 commits ahead of rc.1, all on the web side. The desktop crate
is unchanged — anyone building from this tag gets the same
binary as rc.1 but with the React `/live` dashboard partially on
the new shadcn / Tailwind stack.

## [3.0.0-rc.1] — 2026-05-04

The native-desktop release candidate. `crabcc-desktop` ships with
a complete in-window notification surface, real macOS Notification
Center banners, and an auto-managed docker-compose backend stack
— so on a fresh machine the desktop binary alone is enough to
boot the live dashboard. Major version bump because the desktop
crate is a brand-new top-level surface, not because of breaking
changes to `crabcc serve` / CLI / MCP.

90 commits ahead of `main`; ~30 PRs landed in the wrap-up sprint
on top of the existing dashboard skeleton.

### Added — Track A (desktop dashboard)
- **Six routes** mounted in a header-nav shell: Home, Agents,
  Logs, System, Knowledge, Commands — plus K-Graph (#318, #334,
  #339, #341) and Tool-call Timeline (#312) added in the
  wrap-up sprint.
- **Home dashboard** — KPI strip + activity / agents / services
  tile row + spawn form + force-directed relations graph.
  Activity tile groups consecutive same-op rows, recency-fades
  by age, click op-badge to pin the op-filter (#286, #279).
- **Agents route** — per-card Kill button (#280), substring
  filter + status pills (#283), click-to-expand log tail.
- **Logs route** — telemetry tail with level pills, click any
  row's level badge to drill in (#284).
- **System route** — Services + OTLP + Ollama + profiles +
  models + kills, single filter narrows every section (#283).
- **Knowledge route** — memory drawer browser, ingest form,
  wing-pin filter, wing distribution summary (#285).
- **Commands launchpad** — click-to-run for the 13 no-arg HTTP
  probes that map 1:1 to existing `Client` methods, inline
  result block, type-aware Debug-format dump with copy-to-
  clipboard (#314, #315).
- **A.5.5 relations-graph node-detail drawer** (#300).

### Added — Track C.0 (in-window toast strip)
- 5 levels (Success / Info / Warning / Danger / Primary), per-
  level auto-dismiss intervals from the design brief
  (#308, #309).
- Auto-emit on six submit-result paths (memory ingest / agent
  launch / agent kill × ok / err) and on agent Running → Exited
  transitions (#309, #328).
- Edge-trigger emit on prefetch + telemetry/memory poll
  failures with surgical recovery toasts (one ack per failure
  window, not per cycle) (#310).
- Header `● alerts` mute toggle — clears visible deque,
  history still records (#313).
- Append-only history log capped at 50; footer renders
  `[dismiss all] · history (N) · expand · clear`; click
  `expand` to see the full audit list inline (#316, #319, #340).
- Header `↗ system` toggle for selective Notification Center
  delivery — keep in-window, suppress system banners (#330).
- Per-row `↗ system` tag echoing the system-echo state (#327).

### Added — Track C.1 / C.1.1 / C.2 (native macOS surfaces)
- **Dock badge + menu-bar status item** — running-agents count
  reflected on both surfaces, change-detection sentinels skip
  redundant AppKit round-trips (#272, #281).
- **Rich-notification banners** via `osascript` — every visible
  toast also fires a system banner through Notification Center
  (#325). Sidesteps the `.app` bundle requirement of
  `UNUserNotificationCenter`; future C.2.1 ships a real bundle.

### Added — Backend stack lifecycle
- **Auto-start on launch** — `services::ensure_stack_started`
  runs `docker compose up -d` for the dev stack if `/api/health`
  isn't already responding. Six-variant outcome enum surfaces
  via the toast strip with timing
  (#322, #323, #338).
- **Opt-in graceful shutdown** — SIGINT handler running
  `docker compose down` when
  `CRABCC_DESKTOP_STOP_SERVICES_ON_EXIT=1` (#337).
- **`task services-up / -down / -status / -logs`** — manual
  lifecycle ops (#336).

### Added — Server-side companions
- `/api/seed-graph` nodes now carry `kind` / `file` / `line` /
  `signature` (#320, closes #301).
- `/api/agents/launch` accepts a `profile` field (#331,
  closes #306).
- `SseActivityEvent` carries an `agent_id` (#333, closes #311).

### Performance
- **Bench harness** with criterion — three apply benches +
  graph_layout 50/500-node fixtures (#287).
- **`SharedString` flip** on hot wire types — 10.6× / 5.5× /
  2.4× cumulative wins on apply benches (#288, #289).
- **Cached gpui ElementIds** on `SseAgent` at decode time
  instead of per-render `format!()` (#290).
- **Bounded flume channels** with drop-newest + warn-log
  overflow policy; memory provably bounded (#291).
- **Wild-linker tricks** on DashboardHome render loop — drop
  redundant `.collect::<Vec<_>>()` × 3 + `group_activity`
  buffer reuse (#302).
- **Brand-string cache** in Shell — 2 fewer per-render allocs
  (#304).
- **Per-render `format!()` → `'static str` / `NamedInteger`**
  for alerts toggle + toast dismiss id + activity-op badge id
  — net ~14 fewer heap allocs per render (#342).
- SSE client reuse + thread audit (#282).

### Internal / chores
- Dedicated `desktop-ci.yml` workflow with `paths:` filter so
  the standalone crate gates fmt / check / clippy / bench
  compile without re-running on every workspace change (#292,
  #303).
- Per-crate `crates/crabcc-desktop/Taskfile.yml` (#292).
- Bigger default window 1600×1000 + scrollable body (#305).
- Design brief / architecture docs migrated to private
  `peterlodri-sec/crabcc-docs` submodule under `desktop/`;
  new `desktop/ARCHITECTURE.md` with full data-flow chart
  (#342).
- `crabcc-desktop` is **workspace-excluded** by design —
  `gpui-component`'s `tree-sitter = "0.25"` would force a
  six-grammar coordinated bump on `crabcc-core`'s `0.22`.
  Standalone keeps the gpui ecosystem moving at its own
  cadence.

### Notes for the RC

- C.2 currently attributes notifications to "Script Editor"
  (osascript path). Promotion to `crabcc-desktop` attribution
  + `UNNotificationCategory` actions lands in C.2.1 with a
  proper `.app` bundle.
- Track B (Tailwind / shadcn for `crabcc-viz/web`) — not
  started.
- Apple-Dev-gated tracks (App Group, entitlements, APNs) —
  not started.

## [2.11.0] — 2026-04-30

Operations + ergonomics. macOS-first session focused on the
container layer, the Ollama auth stack's run-time defaults, and
making the keys + state files seamless to the dashboard.

### Added — Apple `container` integration (issue #112 follow-up)
- `install/internal-agents/{Containerfile,compose.yml}` gain
  `init: true`, `mem_limit: 12g`, `cpus: 6`, `cap_drop: ALL`
  (with a minimal re-add list), and SSH-agent passthrough so
  agents can clone via `git@github.com:...` without baked keys.
- `scripts/install-container-completions.sh` — generates
  zsh/bash/fish completion via `container --generate-completion-script`,
  drops it into the right autoload location, writes a fenced alias
  block (`c` / `cps` / `clog` / `cstats`), prints a clear "RELOAD
  CLAUDE CODE" warning at the end.
- `scripts/container-zombie-guard.sh` — host-side janitor
  complementing the in-VM `init: true` reaper. Removes exited > 24h,
  force-stops respawn-loop containers (≥3 consecutive restarts).
- `scripts/install-gitify.sh` — checks for + brew-installs
  [gitify-app/gitify](https://github.com/gitify-app/gitify), the
  open-source macOS GitHub-notifications menubar app. Pairs with
  crabcc's own menubar (different concerns: gitify shows remote
  GH state, ours shows local agent / index / backup state).
  --check / --launch / --json modes; macOS-only.
- README "Useful run-time flags" table for ad-hoc `container run`
  (`--memory`, `--cpus`, `--init`, `--ssh`, `--volume`, cap controls,
  `--publish`, `--rm`).

### Added — Ollama-stack operational tuning (issue #105)
- ollama: 16 GB → 24 GB; new env: `OLLAMA_NUM_PARALLEL=4`,
  `OLLAMA_NUM_THREAD=0`, `OLLAMA_KEEP_ALIVE=30m`,
  `OLLAMA_MAX_LOADED_MODELS=2`, `OLLAMA_FLASH_ATTENTION=1`
- LiteLLM proxy: 1 GB → 2 GB, `--num_workers 1 → 2`, per-model
  `timeout: 600`, `stream_timeout: 60`, `max_parallel_requests: 8`,
  in-memory `cache: type=local, ttl=600`,
  `default_litellm_params: temperature 0.2, top_p 0.9` (matches the
  bundled qwen2.5-coder model-info baseline).
- caddy stays at 256 MB; **dropped security headers** (local
  loopback only, bearer-token gate already covers auth — headers
  were noise). Added `encode zstd gzip`, dial 5s + response-header 5m
  + read/write 10m, `flush_interval -1` for token-by-token streaming,
  `X-Request-Id` correlation forwarded downstream, JSON access log.

### Added — Ollama API key seamlessness
- `init-keys.sh` now WRITES `~/.crabcc.local.api-key` (chmod 0400) on
  every run. Was instructions-only; required users to copy/paste the
  printf+chmod block. The user-facing message reflects whether the
  key was generated this run or pre-existed.
- New `/api/ollama-key` endpoint in crabcc-viz returns
  `{present, path, mode, mtime_secs, size_bytes, key}`. Loopback-only
  dashboard, file already chmod 0400 in $HOME — exposing here is
  no worse than `cat`.
- New `OllamaKeyPanel.tsx` in /live: masked-by-default key display,
  eye-toggle reveal, copy-to-clipboard button (1.5s "copied!"
  feedback), generated-N-hours-ago badge, mode-warn pill if
  permissions aren't 0400.

### Added — Per-stage manual test scripts (issue #105 follow-up)
- `taskfiles/manual-local-stack-setup/` — the 12-section manual
  checklist plus Appendix B.2 decomposed into 14 runnable bash
  scripts, each emitting `✓ PASS` / `✗ FAIL` lines. `lib.sh` is
  the shared header. `run-all.sh` orchestrates with `--keep-going`
  and `--only=03,05` filtering.
- `scripts/verify-agent-chain.sh` — 8-step end-to-end smoke for
  the agent → LiteLLM → Caddy → ollama path. JSON output for
  scripting; `--skip-live` to avoid token spend.

### Added — `internal_agents/macos-sync`
- 6th internal-agent profile (was deferred from #125's set). Owns
  the CLI ↔ macOS menubar feature-parity contract, the .app bundle
  layout, and the Apple `container` files. References gitify as a
  companion app.

### Changed
- Workspace 2.9.0 → **2.11.0** (skipping 2.10.0; reserved for
  PR #128's `feat/backup-crabcc-state` branch which lands separately).

## [2.9.0] — 2026-04-30

Minor bump capturing the Ollama auth-stack work (issue
[#105](https://github.com/peterlodri-sec/crabcc/issues/105)), the
read-only `crabcc doctor` diagnostic surface (issue
[#107](https://github.com/peterlodri-sec/crabcc/issues/107) Phase 5a),
and BullMQ-backed jobs scaffolding (issue
[#109](https://github.com/peterlodri-sec/crabcc/issues/109)). The
user-facing shape: a one-liner `crabcc install-claude
--with-ollama-stack` takes a fresh checkout to a fully-up local
Ollama auth stack with proper Bearer-token auth; `crabcc agent
--backend ollama` routes through it; `crabcc doctor` audits the whole
environment.

### Added — Ollama auth Compose stack (issue #105)
- `install/ollama-stack/` recipe — Caddy reverse proxy enforcing
  `Authorization: Bearer ${OLLAMA_API_KEY}` on `/api` + `/v1`, LiteLLM
  OpenAI-compatible front, Ollama internal-only. `init-keys.sh`
  bootstrap; OCI Image-Spec annotations + `com.crabcc.role` /
  `com.crabcc.issue` labels.
- Docker hygiene: `shm_size: 2gb` on `ollama`, `init: true`,
  `cap_drop: ALL`, `read_only: true` on Caddy, `tmpfs /tmp`,
  per-service memory caps, log rotation.
- `install/init-shared-network.sh` — idempotent bootstrap of the
  cross-stack `crabcc-shared` external bridge network.
- `install/Dockerfile.crabcc` — multi-stage with BuildKit cache
  mounts.
- `install/dev/docker-compose.yml` — local-dev convenience stack.
- `install/ollama-stack/OLLAMA-AUTH.md` + `MANUAL_TEST_CHECKLIST.md`
  (12-section e2e gate).

### Added — `crabcc ollama-stack` operator subcommand (issue #105 Phase 3)
- `up`/`down`/`status`/`logs`/`pull` with JSON output for
  machine-consumer surfaces.
- `ccc setup --ollama-{up,down,down-volumes,status,pull}` shortcuts.

### Added — `crabcc agent --backend ollama` (issue #105 Phase 4)
- `--backend <claude|ollama>` (default `claude`). Ollama path runs
  `ensure_up()` before spawn; subprocess inherits `OLLAMA_BASE_URL` +
  `OLLAMA_API_KEY`. Default model `ollama/qwen2.5-coder` (code-tuned
  per `litellm.config.yaml`'s `model_list`). `meta.json` records the
  backend.

### Added — `crabcc doctor` diagnostic surface (issue #107 Phase 5a)
- `crabcc doctor [docker|stack|keys|agent|jobs] [--text]` —
  read-only environment audit. JSON-default for menubar/extension;
  `--text` for humans. No subcommand = aggregated `DoctorReport`
  with `overall: ok|warn|fail` (exits 1 on Fail).
- 14 unit tests across the surface; OS-aware install hints
  (OrbStack-preferred on macOS).

### Added — `install-claude --with-ollama-stack` (issue #105 Phase 5b)
- Materializes the embedded Compose recipe (7 files, ~30 KB via
  `include_bytes!`) to `~/.crabcc/ollama-stack/`, runs
  `docker compose up -d --wait`, reports services healthy.
  Idempotent.
- `--print-stack-instructions` counterpart to `--print-hooks`.

### Added — `crabcc upgrade --with-stack` (issue #105 Phase 5b)
- Refreshes the bundled stack alongside the version check.
  `--check --with-stack` for read-only dry-run; `--apply
  --with-stack` does `compose pull && up -d --wait`.

### Added — Single-file YAML config (issue #105)
- `~/.crabcc/._config.internal` (overridable via `$CRABCC_CONFIG`).
  Default-on (no feature flag). `Config { agent, ollama, jobs, mcp }`
  with sensible defaults; `deny_unknown_fields` everywhere so typos +
  version drift surface at parse. Atomic `save()` (mode 0600 on Unix,
  banner comment auto-prepended). 8 unit tests.

### Added — BullMQ-backed jobs scaffolding (issue #109)
- `crabcc-core::jobs` gated behind the `jobs` cargo feature. `tokio`
  + `redis` enter the workspace through this feature only — not
  workspace-wide (per issue #112's methodology).
- `submit_async` encodes `JobSpec` in BullMQ's on-disk Redis layout:
  INCR `bull:<queue>:id`, MULTI/EXEC HSET + LPUSH/ZADD onto
  `bull:<queue>:wait` (or `:delayed`). Two-phase atomicity matches
  BullMQ's own Lua scripts.
- `status_async` walks `wait` / `active` / `delayed` / `completed` /
  `failed` keys to look up a job's current state. Returns
  `JobStatus::Unknown` when not found.
- `apps/jobs-worker/` — Bun + TypeScript BullMQ Worker, one Worker
  per queue (`agent:run` / `agent:flow` / `repo:index` /
  `repo:reindex`). Today's handler is an echo passthrough; real
  handlers in follow-up. Multi-stage Dockerfile + dev-compose
  service under the `jobs` profile.

### Added — Performance plumbing (issue #112)
- `task pgo-release` — cargo-pgo three-pass with host-triple-detected
  training corpus. Typical 10–20% gain on CLI hot paths.
- `task build-native` — `RUSTFLAGS=-C target-cpu=native` opt-in
  (personal/dev only — produces non-portable binaries).
- `[profile.release-native]` Cargo profile.

### Added — Devx
- `scripts/setup-gpg-signing.sh` + `task gpg-signing` — repo-local
  ed25519 commit-signing setup. Idempotent: `--rotate` / `--print` /
  `--uninstall`. Public key auto-copied to clipboard.
- `task images-build` / `images-build-nocache` / `images-inspect` —
  Docker image pipeline with OCI labels (git revision + ISO-8601
  timestamp + workspace version).

### Changed — Error hints route to `crabcc doctor`
- `agent --backend ollama` failure paths now suggest
  `crabcc doctor docker` / `crabcc doctor stack` for diagnosis.
- `install-claude --with-ollama-stack` and `upgrade --with-stack`
  failures suggest `crabcc doctor` / `crabcc doctor stack`.

### Out of scope (next PRs)
- `apps/jobs-worker` real per-queue handlers (today's echo proves the
  wire round-trip).
- Repeatable jobs + flows in `crabcc-core::jobs`.
- macOS menubar telemetry consumer for jobs-worker logs.
- macOS menubar app + Chrome extension UI work (issue #107 Parts
  A/B).
- BullMQ wire-protocol priority-queue ZADD (today priority is
  encoded in the hash; worker still respects it).
- SIMD intrinsics for hot loops (gated on flamegraph evidence per
  issue #112's methodology).

## [2.8.0] — 2026-04-30

Minor bump capturing the macOS installer surface (issue [#107](https://github.com/peterlodri-sec/crabcc/issues/107))
plus agent-run lifecycle tracking. The `Crabcc.app` bundle is the
user-facing artifact: a menubar app surfacing live process state,
scheduled LaunchAgent tasks, and recent kill events from a singleton
SQLite store that every `crabcc agent` invocation now records into.

### Added — macOS installer .app + DMG (issue #107)
- `installer/Crabcc.app/` — drag-to-install bundle with `LSUIElement`,
  `com.crabcc.installer` ID, ad-hoc codesigned so it shows up in
  System Settings → Privacy & Security → App Management.
- Menubar UI (single-file `menubar.swift`, compiled at build time with
  `swiftc` — no Xcode project): live status (indexes / watches / agents
  / agentd), Run Task submenu populated from `Taskfile.yml`, Scheduled
  Tasks submenu, Recent Kills submenu, About panel, Reindex Now / Run
  Guard Now / Open Logs / Reinstall actions, JSON-lines telemetry.
- Three LaunchAgents installed by the bundle:
  - `com.crabcc.menubar` — RunAtLoad + KeepAlive-on-crash, Interactive
    QoS so the menubar stays responsive after sleep/wake/restart.
  - `com.crabcc.agentd` — KeepAlive=true background tick, every 5 min
    runs `crabcc refresh` against repos in `~/.crabcc/agent/repos.list`.
    Pure shell with `trap 'wait' SIGCHLD` for zombie-free child reaping.
  - `com.crabcc.agent-guard` — `StartInterval=1200` (every 20 min)
    background sweep.
- `scripts/build-dmg.sh` — stage Crabcc.app, populate Resources with
  release-mode `crabcc`/`ccc` + skills/ + commands/ + install-aliases.sh,
  swiftc the menubar shim, ad-hoc codesign the bundle, hand off to
  `hdiutil` → `dist/crabcc-<version>.dmg` (~8.6 MB UDZO compressed).
- `task dmg` Taskfile target wires the above end-to-end.

### Added — bootstrap + helper scripts
- `scripts/bootstrap.sh` — `curl | bash`-able fresh-machine setup:
  preflight (rustup if missing), clone into `~/workspace/bin/crabcc`,
  cargo install, ad-hoc codesign (Sequoia provenance fix), aliases,
  skill/command symlinks, optional `--with-docker` / `--with-launchd` /
  `--with-macos-app`. Idempotent — same script for fresh + upgrade.
- `scripts/install-macos-helpers.sh` — register/remove the
  `com.crabcc.agentd` LaunchAgent against the local repo without
  building the DMG. Useful for dev-machine smoke + CI.
- `scripts/doctor.sh` — added `macos-app` + `launch-agent` checks;
  `--install` now renders + bootstraps the LaunchAgent when Crabcc.app
  is installed.

### Added — agent run lifecycle DB + guard (issue #107)
- `~/.crabcc/_internal.db` — singleton SQLite (WAL) recording every
  `crabcc agent` invocation: PID, repo, runtime, model, log path,
  exit code, timestamps. `agent_runs` + `agent_kill_events` tables.
  `agent.rs` writes on start (insert + update_pid) and on exit
  (mark_finished). Best-effort — bookkeeping never fails the run.
- `crabcc agent-ls [--active-only] [--json]` — lists rows. Reaps
  zombies (rows still 'running' whose PID is gone) on every call.
- `crabcc agent-guard [--idle-secs N] [--json]` — periodic janitor
  fired by the `com.crabcc.agent-guard` LaunchAgent every 20 min.
  Detects zombies (PID gone) and stuck runs (log mtime older than
  `--idle-secs`, default 1800). SIGTERM with 5 s grace, then SIGKILL.
  Records every action in `agent_kill_events` and writes a per-run
  `~/.crabcc/agents/<id>/.agent-<id>-kill-log` audit file.
- `crabcc agent-kills [--json]` — lists kill events for the menubar +
  future viz dashboard "incidents only" filter.

### Changed
- `Reinstall / Update…` menubar action now fetches the latest
  `scripts/bootstrap.sh` from GitHub via `gh` (curl fallback) and runs
  it — the update path can never get stuck on stale bundled binaries.
- `README.md` gained "Bootstrap a fresh machine" + "macOS .app + DMG
  installer" sections.

## [2.7.0] — 2026-04-30

Minor bump capturing the post-2.6.0 wave: ollama-backed audit sub-agents,
three new skills (warp-speed-audit, rust-logging-audit, crabcc-taskfile),
the `ccc` combo CLI binary, the `/ccc-init:lazy` bootstrap, structured
tracing across hot paths, a CI scoping pass that drops PR-time
wall-clock, and a `gen-summary` step now wired into `prep-pr` and the
pre-commit hook.

### Added — local Ollama (OpenClaw) backend for audit sub-agents (PR [#100](https://github.com/peterlodri-sec/crabcc/pull/100))
- `scripts/ollama-fanout.sh` — parallel `/api/generate` fan-out
  (default `--parallel 4`); JSON-array or JSONL prompts; merged JSON
  output. `--json-mode` for strict-JSON replies.
- `scripts/ollama-system-check.sh` — local pre-flight (arch / RAM /
  disk / daemon) with per-model requirements baked in.
- `scripts/ollama-network-check.sh` — remote pre-flight (DNS / mDNS /
  TCP / `/api/version` / `/api/tags` / model presence; optional
  `--smoke` adds a 1-token round-trip).
- `scripts/ollama-agent-runtime.sh` — single-shot wrapper for
  `crabcc agent --run --llm ollama`. Reads `.tool_calls` +
  `.thinking` + `.response` to handle gpt-oss / OpenClaw model
  conventions; `format:"json"` deliberately NOT set (clobbers native
  fields on thinking models).
- Six new Taskfile entries: `system-check`, `ollama-network-check`,
  `ollama-bootstrap`, `ollama-smoke`, `ollama-models`,
  `agent-runtime-smoke`.
- Default model `voytas26/openclaw-oss-20b-deterministic` (gpt-oss:20b
  fine-tune for OpenClaw autonomous agents); override via
  `CRABCC_OLLAMA_MODEL=…`.
- Both `warp-speed-audit` and `rust-logging-audit` skills get a
  Phase-2 rewrite: orchestrator runs the `crabcc` probes locally,
  fans analysis to local Ollama in parallel; Claude-Agent fallback
  retained for hosts that fail `system-check`.

### Added — `gen-summary` task wired into prep-pr + pre-commit hook (PR [#100](https://github.com/peterlodri-sec/crabcc/pull/100))
- `scripts/gen-summary.sh` — generate a paste-ready markdown summary of
  the current branch (commits ahead of base, files changed by crate,
  issue references, prep-pr gate status). Output:
  `.summary/gen-summary.md`. Pure git ops; <500 ms typical.
- `task gen-summary` — invokes the script.
- `task prep-pr` now calls `gen-summary` as a final step (after
  fmt + clippy + test + doc); the resulting markdown is paste-ready
  for `gh pr create --body-file`.
- Pre-commit hook (`scripts/git-hooks/pre-commit`) refreshes
  `.summary/gen-summary.md` after the fmt + clippy gate so a `gh pr
  create` immediately after commit can pick it up.

### Added — rust-logging-audit skill + tracing migration RFC (PR [#92](https://github.com/peterlodri-sec/crabcc/pull/92), issue [#90](https://github.com/peterlodri-sec/crabcc/issues/90))
- `skill/rust-logging-audit/SKILL.md` — sister skill to
  `warp-speed-audit`. Audits a Rust repo for `tracing` adoption,
  hot-path discipline, init-time hygiene, framework mix.
- `skill/rust-logging-audit/MIGRATION-RFC.md` — concrete 4-phase
  plan for migrating crabcc to `tracing` + `tracing-opentelemetry`,
  with rotel (issue [#86](https://github.com/peterlodri-sec/crabcc/issues/86) `/live` panel) as the OTLP terminus.

### Added — crabcc-taskfile skill + auto-bootstrap aliases (PR [#93](https://github.com/peterlodri-sec/crabcc/pull/93))
- `skill/crabcc-taskfile/SKILL.md` routes intent
  (build / test / lint / bench / release / install) onto
  `task <name>` entries.
- `task install` now idempotently bootstraps shell aliases via
  `scripts/install-aliases.sh`. Pass `NO_ALIASES=1` to opt out.
- `task aliases-check` is read-only detection.

### Added — observability: structured `tracing` across hot paths (PR [#99](https://github.com/peterlodri-sec/crabcc/pull/99))
- info-level entry/exit logs in `crabcc-core` (full_index,
  refresh_delta, `Store::open_with_compress`) and per-command logs
  in `crabcc-cli::main` via a stable `cmd_name_for_log()` mapping.
- debug-level per-query detail in `query::find_symbol`,
  `query::query_callers`, `query::query_refs` (counts + path +
  elapsed_ms). Gated behind `RUST_LOG=debug`.

### Added — ccc combo CLI + lazy bootstrap (PRs [#96](https://github.com/peterlodri-sec/crabcc/pull/96), [#98](https://github.com/peterlodri-sec/crabcc/pull/98), issue [#74](https://github.com/peterlodri-sec/crabcc/issues/74))
- `ccc` — high-level combo CLI binary that fronts the most-used
  crabcc verbs with shorter alias routing.
- `/ccc-init:lazy` slash command performs the full repo bootstrap
  (index / graph / memory / mine / aliases / tools / serve / watch /
  guard / ollama / upgrade / marker) in one shot.

### Added — slash-command install / upgrade plumbing (PR [#95](https://github.com/peterlodri-sec/crabcc/pull/95))
- `/crabcc-upgrade` and `/crabcc-install` slash commands wired into
  the install flow.

### Changed — CI scope on PRs (PR [#91](https://github.com/peterlodri-sec/crabcc/pull/91), Pillar 3 of issue [#87](https://github.com/peterlodri-sec/crabcc/issues/87))
- PRs run `cargo clippy` only on changed crates; `cargo fmt --check`
  only on changed `*.rs` files; smoke step skips unless
  `crabcc-cli` or `crabcc-core` changed; `aliases-smoke` only when
  `scripts/install-aliases.sh` changed.
- `push` to `main` keeps the full belt-and-suspenders matrix.

### Changed — release pipeline (PR [#94](https://github.com/peterlodri-sec/crabcc/pull/94))
- `[profile.dev]` tuned for faster iteration.
- UPX compression dropped from the release pipeline.

### Changed — repository hygiene (PR [#97](https://github.com/peterlodri-sec/crabcc/pull/97))
- Drop unused `.devcontainer/` config.
- README TOC + asset workflow note.

### Changed — workspace version bump
- `Cargo.toml` `[workspace.package].version` → `2.7.0`.
- `.gitignore` adds `.summary/` (per-developer task scratch dir).

## [2.6.0] — 2026-04-30

Minor bump capturing post-v2.5.0 work that stacked on `main` while the
v2.5.0 GitHub release artifacts were blocked by an upstream billing
issue. Treated as a coherent feature drop rather than a v2.5.x patch
because of the volume of net-new surface (live viewer, agent runtime,
MCP dev gate, query envelope features, perf pass).

### Added — agent runtime ([#62](https://github.com/peterlodri-sec/crabcc/issues/62), PR [#69](https://github.com/peterlodri-sec/crabcc/pull/69))
- `crabcc agent --run "<prompt>"` host-subprocess runtime. Default
  model `claude-opus-4-7`; per-run state at `~/.crabcc/agents/<id>/`
  (lock, pid, log, meta.json); `--dry-run` for wiring checks;
  auth pass-through to existing `~/.claude/` config.
- `[features] agent-sandbox` cargo feature on `crabcc-cli` exposes
  a `SandboxRuntime` stub returning `not yet implemented (#62)` —
  microsandbox-backed runtime drops in as a single feature flip
  for v3.0.
- `install/agent-runtime.md` documents today vs. v3.0 trust
  boundary + cross-platform story.

### Added — localhost call-graph viewer ([#64](https://github.com/peterlodri-sec/crabcc/issues/64), PRs [#66](https://github.com/peterlodri-sec/crabcc/pull/66) / [#68](https://github.com/peterlodri-sec/crabcc/pull/68) / [#72](https://github.com/peterlodri-sec/crabcc/pull/72))
- `crabcc serve [--port 7878] [--no-open]` — single self-contained
  HTML page with a force-directed graph rendered on Canvas/WebGL.
  Bound to `127.0.0.1` only by default; zero external network
  calls; assets bundled.
- WebSocket push on `crabcc refresh` so the open page re-renders
  without a manual reload.
- `/live` agent-activity overlay (PR #72) — live monitoring
  dashboard with auto-init + agent-bin/PATH plumbing.

### Added — MCP dev gate ([#59](https://github.com/peterlodri-sec/crabcc/issues/59) slice, PR [#70](https://github.com/peterlodri-sec/crabcc/pull/70))
- `tools/list` no longer ships `_openapi` or `_health` to the
  default agent-facing surface — restore the full diagnostic
  surface with `crabcc --mcp --dev` or `CRABCC_MCP_DEV=1`.
- New `pub fn handle_with(req, root, dev)`, `serve_stdio_with(root, dev)`,
  `tools_def_for(dev)`. Calling a meta tool on the default surface
  returns a JSON-RPC error pointing at `--dev`.

### Added — query-surface envelope features (PR [#67](https://github.com/peterlodri-sec/crabcc/pull/67))
- `--if-changed FINGERPRINT` cache-revalidation envelope on
  `refs` / `callers`. Match → `{unchanged, fingerprint}`; mismatch
  → `{fingerprint, result}`. Default omitted = byte-identical to
  existing surface.
- `crabcc refresh --delta` returns `{added, modified, removed,
  stats}` instead of bare counts. Lists are sorted (byte-stable
  output). `touched` (mtime-only) intentionally excluded.
- `--since SHA` filter on `sym` / `refs` / `callers` (CLI + MCP).
  Resolves via `git diff --name-only --diff-filter=AMR
  <since>...HEAD` (new `crabcc_core::gitdiff` module). Filter
  applied before any IO.
- `--ndjson` / `stream: true` on `refs` / `callers` — NDJSON
  output (one hit per line) for streaming consumers. Hits-mode only.

### Added — devx: local CI runner + profiling (PRs [#65](https://github.com/peterlodri-sec/crabcc/pull/65), [#70](https://github.com/peterlodri-sec/crabcc/pull/70))
- `scripts/local-ci.sh` + `task local-ci{,-quick,-release}` —
  canonical "skip GitHub CI" runner. Mirrors `.github/workflows/ci.yml`
  + prep-pr extras with a single pass/fail summary table.
- `[profile.profiling]` Cargo profile — release-like with `debug = true`
  so profilers see symbols.
- `task profile-samply CMD=...` and `task profile-flamegraph CMD=...`
  install + run the profiler against the chosen subcommand.

### Added — perf pass (PR [#71](https://github.com/peterlodri-sec/crabcc/pull/71))
- `ahash::AHashMap` replaces std `HashMap` on hot paths
  (`Store::list_files_with_meta`, `query::build_summary`).
- `#[inline]` on `backend::cosine` (dispatcher above the SIMD/scalar
  split) and `Codec::decompress` (per-signature decode on every
  read). Helps LTO across crate boundaries / FFI seams.

### Changed — workspace version bump
- `Cargo.toml` `[workspace.package].version` → `2.6.0`.
- `Cargo.lock` regenerated.

## [2.5.0] — 2026-04-30

First tagged release of the 2.5 line. The version was set in
`Cargo.toml` when M1 hybrid search landed (#48); v2.5.0 now ships
the full body of work that has stacked on `main` since.

### Added — agent-friendly query surface (#63, #67)
- `--summary` mode on `refs` / `callers` (and MCP `mode: "summary"`).
  Returns `{by_file, top_files, top_symbols}` — distribution shape
  for agents that need shape, not individual matches. ~95% bytes
  saved vs raw hits. `top_symbols` resolves the innermost enclosing
  symbol per hit via `Store::symbols_in_file` so agents see *which
  functions or classes* contain the matches.
- `--if-changed FINGERPRINT` cache-revalidation envelope on
  `refs` / `callers`. Match → `{unchanged: true, fingerprint}`.
  Mismatch → `{fingerprint, result}`. Default omitted = byte-identical
  to existing surface.
- `crabcc refresh --delta` — returns `{added, modified, removed,
  stats}` instead of bare counts. Lists are sorted (byte-stable
  output). `touched` files (mtime-only changes) intentionally
  excluded.
- `--since SHA` filter on `sym` / `refs` / `callers` (CLI + MCP).
  Resolves via `git diff --name-only --diff-filter=AMR
  <since>...HEAD` (new `crabcc_core::gitdiff` module — shells out
  to user's `git`; no libgit2 dep). Filter is applied *before* any
  IO — walker skips non-matching files, edges path filters
  `edge_hits` upfront.
- `--ndjson` (CLI) / `stream: true` (MCP) on `refs` / `callers` —
  NDJSON output (one hit per line) for streaming consumers.
  Hits-mode only.

### Added — localhost call-graph viewer (`crabcc serve`, closes [#64](https://github.com/peterlodri-sec/crabcc/issues/64)) (#66)
- `crabcc serve [--port 7878] [--no-open]` — single self-contained
  HTML page with a force-directed call-graph rendered on Canvas.
  Bound to `127.0.0.1` only. Zero external network calls; assets
  bundled in the binary.
- HTTP routes: `GET /` (page), `GET /api/graph?root_symbol&depth&dir`,
  `GET /api/sym?name`, `GET /api/files?under`. `WS /api/live` pushes
  re-render hints when `crabcc refresh` runs in another shell.

### Added — devx: profiling profile + Taskfile targets (#65)
- New `[profile.profiling]` in Cargo.toml — release-like with
  `debug = true` so profilers see the symbols.
- Taskfile targets: `task profile-samply CMD=...` and
  `task profile-flamegraph CMD=...` install + run the profiler
  against the chosen subcommand.

### Added — bench rerun + README refresh (closes [#28](https://github.com/peterlodri-sec/crabcc/issues/28)) (#56)
- Re-ran `task bench-compress` and the e2e benchmarks against the
  v2.5.0-shaped index; refreshed README headline numbers.

### Fixed — Claude Code hooks integration (closes [#29](https://github.com/peterlodri-sec/crabcc/issues/29)) (#61)
- `hooks-claude.json`: stdin / `jq` plumbing corrected,
  SessionStart matcher uses an anchored regex.

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
