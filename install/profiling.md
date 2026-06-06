# Profiling crabcc

These Taskfile targets cover the daily-driver perf-investigation workflow. None are wired into `task ci` — profiling is on-demand by design.

| Target | Tool | Sudo? | Output |
|---|---|---|---|
| `task profile-build` | cargo | no | `target/profiling/crabcc` (release codegen + line-table debuginfo) |
| `task profile-index REPO_FIXTURE=PATH` | [samply](https://github.com/mstange/samply) | no (macOS + Linux) | `.summary/profile-index.json` (open with `samply load`) |
| `task profile-memory-search [N=200] [Q="..."]` | samply | no | `.summary/profile-memory-search.json` |
| `task profile-fts [N=200] [Q="..."]` | samply | no | `.summary/profile-fts.json` |
| `task flamegraph-index REPO_FIXTURE=PATH` | [cargo-flamegraph](https://github.com/flamegraph-rs/flamegraph) | **yes** on macOS (DTrace) | `.summary/flamegraph-index.svg` |

## Profiling the symbol search path

Fuzzy/prefix lookup is now a native SQLite-backed scan (no Tantivy). Two angles:

- **End-to-end, via the CLI** — `task profile-fts` indexes `REPO_FIXTURE`, then samples N hot `crabcc lookup fuzzy/prefix` calls. Each call rebuilds the in-memory `Fts` from the store and scans it, so the profile shows the real split between `Store::iter_symbol_names` (the name-only load) and the bounded-Levenshtein / prefix scan.
- **In-process microbench** — `cargo bench -p crabcc-core --features bench --bench fts_search` runs a criterion matrix of corpus size (1k / 10k / 50k) × query shape (fuzzy exact/typo1/typo2/nomatch, prefix broad/narrow) plus `Fts::from_symbols` build cost. Use this to compare scan implementations without SQLite or process-spawn noise. The `fuzzy_prefix_scale_roughly_linearly` unit test guards the same path against an accidental O(N²) regression on every `cargo test`.

`samply` is the recommended default — it works on macOS without sudo, supports both DTrace and the kernel's mach task ports, and ships a browser-based flamegraph viewer with much better zoom + symbol resolution than a static SVG. Reach for `cargo-flamegraph` only when you specifically want a single SVG to attach to an issue.

## Cargo `profiling` profile

Defined in the workspace `Cargo.toml`:

```toml
[profile.profiling]
inherits      = "release"          # opt=3, LTO=fat, codegen-units=1
debug         = "line-tables-only" # smallest debuginfo that still resolves symbols
strip         = false              # required so the profiler sees fn names
```

`line-tables-only` keeps the binary close to release size while making sure samply / flamegraph / Instruments don't show `<unknown>` for every frame. If you need full debug info (e.g. for a step-debugger session), pass `RUSTFLAGS="-C debuginfo=2"` on top.

## Recipe — index profile

```bash
# 1. Build the profiling binary (release codegen + symbols).
task profile-build

# 2. Capture a profile against the fixture repo.
REPO_FIXTURE=/path/to/your/repo task profile-index

# 3. Open in samply's web viewer (browser tab; no upload).
samply load .summary/profile-index.json
```

The Taskfile wipes `<REPO_FIXTURE>/.crabcc/` before recording so the profile measures the cold-index path (parse + extract + insert), not a warm-cache no-op.

## Recipe — memory.search profile

```bash
# Records 1000 inserts then N=200 hot search() calls.
N=500 Q="fox jumps" task profile-memory-search
samply load .summary/profile-memory-search.json
```

Useful for measuring the relative weight of: cosine cycle, FTS5 BM25, RRF fusion, sonic-rs encode. With `--features memory-embed` set the inserts will go through `FastEmbedder` (ONNX) and the profile will look completely different.

## Recipe — flamegraph SVG (issue-attachable)

```bash
sudo task flamegraph-index REPO_FIXTURE=/path/to/your/repo
# .summary/flamegraph-index.svg
```

Sudo is required on macOS because `cargo-flamegraph` shells out to DTrace. On Linux it's `perf` and the requirement is governed by `kernel.perf_event_paranoid` — sudo is the path of least resistance.

## What to look for

When profiling the indexer, the top hot frames should be (rough expectation against a real Rails repo):

1. `tree_sitter::Parser::parse` — bulk of the time; the C grammar
2. `crabcc_core::extract::walk` — Rust dispatch over AST nodes
3. `rusqlite::Connection::execute` — symbol/edge inserts
4. `crabcc_core::store::Store::insert_*` — wrapper logic + FSST encode

For memory.search, expect:

1. `crabcc_memory::backend::cosine` — vector distance loop (target for `--features simd-cosine`)
2. `crabcc_memory` FTS5 BM25 — lexical scoring in SQLite
3. `crabcc_memory::palace::rrf_fuse` — small but visible
4. `sonic_rs::to_string` — encode tail (should be < 5%)

If any of those are wildly out of order, the profile is the canonical signal that something needs investigation.

## Diff-profiling

Two profiles, one before and one after a change, taken with the same fixture + same N:

```bash
task profile-build
REPO_FIXTURE=/path/to/repo task profile-index
mv .summary/profile-index.json .summary/profile-before.json

# … apply your change, rebuild …
task profile-build
REPO_FIXTURE=/path/to/repo task profile-index
mv .summary/profile-index.json .summary/profile-after.json

# Open both in samply and compare totals + per-fn deltas.
samply load .summary/profile-before.json
samply load .summary/profile-after.json
```

Issue #57 (perf review) lists the next-wave candidates and is the canonical place to attach profile snapshots when proposing a perf PR.

## Caveats

- `samply` and `cargo-flamegraph` measure wall-clock time, not CPU time. Sleeping, I/O, and locks all show up. For pure CPU hotspots reach for Instruments.app's "Time Profiler" template on macOS.
- `task profile-build` produces a binary with `lto = "fat"` so symbols can collapse across crate boundaries — sometimes the function you're looking for has been inlined into its caller. Drop to `inherits = "release"` + `lto = false` in `Cargo.toml` temporarily if you need to see the full inlining tree.
- The `.summary/` directory is `.gitignore`'d (per #53). Profiles never accidentally end up in a PR — paste their highlights into the PR body or attach the SVG to a comment.
