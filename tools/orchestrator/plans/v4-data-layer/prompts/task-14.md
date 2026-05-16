# Task 14 — Release prep: version 4.0.0, CHANGELOG entry, AGENTS.md migration note

## Context

Wave 3, parallel. The data-layer rewrite (Tasks 1–13) lands a breaking
schema change (symbol-ID-keyed edges), three new first-class resolvers
(Rust / JS-TS / Python), and four new graph ops. That earns a major
version bump from 3.2.0 → 4.0.0.

Three files to edit, all surgical:

1. `Cargo.toml` — bump the workspace version.
2. `CHANGELOG.md` — add a `## [4.0.0]` section between the `[Unreleased]`
   placeholder and the existing `[3.2.0]` entry.
3. `AGENTS.md` — append a one-bullet migration note inside the existing
   "Conventions agents should respect" section.

## What to change

### File 1: `Cargo.toml`

Find this exact two-line block (lines 30–31):

```toml
[workspace.package]
version = "3.2.0"
```

Replace it with:

```toml
[workspace.package]
version = "4.0.0"
```

That is the only change to `Cargo.toml`. Do not touch any
`[workspace.dependencies]` entry, any `[patch]` section, or anything else.

### File 2: `CHANGELOG.md`

Find this exact block at the top of the file:

```markdown
## [Unreleased]

## [3.2.0] — 2026-05-16
```

Replace it with:

```markdown
## [Unreleased]

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
```

The rest of `CHANGELOG.md` (the existing `[3.2.0]` body and everything
older) is untouched.

### File 3: `AGENTS.md`

Find this exact block in the "Conventions agents should respect" section
(around lines 184–186):

```markdown
- **One feature, one PR.** Don't fold release prep, refactors, and a feature
  into a single commit.
```

Replace it with:

```markdown
- **One feature, one PR.** Don't fold release prep, refactors, and a feature
  into a single commit.
- **v4.0 schema change.** Opening a pre-v4 index on v4.0.0+ wipes and
  rebuilds it on the first command (~60 s on this 13k-file repo). The
  banner reads `crabcc: index built with schema v3; wiping and
  re-indexing for symbol-ID edges...` — same shape as the v3.2 upgrade
  banner. There is no migrator, no opt-out flag, and no user choice; the
  rebuild is gated by a `schema_v4_built` meta key. Agents that script
  against `crabcc` should expect the first call after upgrade to take
  rebuild-time, not query-time.
```

The rest of `AGENTS.md` (everything before that bullet, every other
section) is untouched.

## Definition of done

- `Cargo.toml` reports `version = "4.0.0"` under `[workspace.package]`.
- `CHANGELOG.md` has a `## [4.0.0] — 2026-05-16` section with the
  Breaking / Added / Changed subsections shown above, placed between the
  `[Unreleased]` placeholder and the `[3.2.0]` entry.
- `AGENTS.md`'s "Conventions agents should respect" section gains the
  "v4.0 schema change" bullet as the new final item.
- No other file in the repo is touched.

Do not run `cargo build`, `cargo test`, or any other build or test command.

Do not modify any other file. Do not invent extra files.

Then commit with this exact message:

    release: bump to 4.0.0 + CHANGELOG entry + AGENTS migration note
