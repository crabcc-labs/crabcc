# Architecture

> Status: living document. Companion to [README.md](../README.md), the
> compression research note in [RESEARCH-fsst.md](RESEARCH-fsst.md), and the
> prior-art storage research in [RESEARCH-mempalace.md](RESEARCH-mempalace.md).
>
> Audience: contributors landing changes in `crates/crabcc-core` or wiring a
> new editor / agent integration through `crates/crabcc-mcp`.

## At a glance

```
                   ┌────────────────────────────────────────────────┐
                   │                Editor / Agent                  │
                   │   (Claude Code, Cursor, plain shell, CI…)      │
                   └───────────────┬────────────────────────────────┘
                                   │ stdin/stdout (text or JSON-RPC)
                                   ▼
                   ┌────────────────────────────────────────────────┐
                   │            crabcc CLI  (crabcc-cli)            │
                   │   subcommands: index | refresh | sym | refs |  │
                   │   callers | files | outline | watch | --mcp …  │
                   └───────┬────────────────────────────┬───────────┘
                           │ in-proc                    │ in-proc (--mcp)
                           ▼                            ▼
                   ┌──────────────────┐         ┌────────────────────┐
                   │   crabcc-core    │◀────────│    crabcc-mcp      │
                   │   (lib)          │  reuses │  (JSON-RPC routes) │
                   └───────┬──────────┘         └────────────────────┘
                           │
            ┌──────────────┼──────────────────────┐
            ▼              ▼                      ▼
   ┌──────────────┐  ┌──────────────┐    ┌────────────────────┐
   │  SQLite DB   │  │ Tantivy 0.22 │    │   tree-sitter      │
   │ .crabcc/     │  │ .crabcc/     │    │  parsers (TS, JS,  │
   │  index.db    │  │  tantivy/    │    │  TSX, Ruby)        │
   │ (WAL, mmap)  │  │ (FTS sidecar)│    │                    │
   └──────────────┘  └──────────────┘    └────────────────────┘
            │              │
            └──────┬───────┘
                   ▼
           CLI / MCP outputs
           (text · JSON · JSON-RPC)
```

The editor (or any other client) talks to the `crabcc` binary. Almost all
real work lives in the `crabcc-core` library — both the CLI and the MCP
server are thin façades on top of it. Persistent state is a per-repo
`.crabcc/` directory holding a SQLite database (the source of truth) and a
Tantivy sidecar (rebuildable from SQLite at any time).

## Workspace layout

The repository is a Cargo workspace with three crates:

| Crate          | Kind | What it does                                                              |
| -------------- | ---- | ------------------------------------------------------------------------- |
| `crabcc-core`  | lib  | Indexing pipeline, query API, storage, FTS, tree-sitter glue, watch loop. |
| `crabcc-cli`   | bin  | Argument parsing (`clap`), output formatting, subcommand dispatch.        |
| `crabcc-mcp`   | lib  | JSON-RPC routes that re-export `crabcc-core` queries to MCP clients.      |

The CLI binary embeds `crabcc-mcp` and switches into MCP mode when the user
passes `--mcp` (stdio JSON-RPC, intended to be spawned by Claude Code or
similar). There is no separate `crabcc-mcp` binary — keeping it as a library
lets us reuse the CLI's argument validation and error formatting.

Top-level dirs worth knowing about:

```
crates/             cargo workspace members
schema/             SQLite DDL — single source of truth, embedded via include_str!
docs/               this file + the RESEARCH-*.md design notes
bench/              perf harnesses (raw-bench.py, codeindex-vs-crabcc.py, …)
install/            packaging glue (Claude hooks, brew formula stubs)
commands/, skill/   Claude Code skill + slash command shipped with crabcc
```

## Data model (SQLite)

The schema is one file, [`schema/001_init.sql`](../schema/001_init.sql),
embedded into the `crabcc-core` binary via `include_str!`. Four tables:

| Table     | Purpose                                                                       |
| --------- | ----------------------------------------------------------------------------- |
| `files`   | One row per indexed file: `path`, `sha256`, `mtime`, `lang`, `indexed_at`.    |
| `symbols` | Definitions: `name`, `kind`, `signature`, `parent`, line range, `visibility`. |
| `edges`   | References / call sites: `dst_name`, `kind` (`call`/`import`/`inherit`/…).    |
| `meta`    | Key-value store, currently holds `schema_version`.                            |

Indices are tuned for the actual access patterns the CLI exercises:

- `idx_symbols_name` and `idx_symbols_name_kind` cover `crabcc sym <name>`.
- `idx_symbols_file_line` is a covering index for `crabcc outline FILE`
  (avoids a sort).
- `idx_files_lang` makes `crabcc files --lang ruby` a constant-time SQL.
- `idx_edges_dst` covers `crabcc refs` / `crabcc callers`.

### `signature_enc` — the v2.0 hook

The `symbols` table carries a forward-looking column: `signature_enc INTEGER
NOT NULL DEFAULT 0`. While `0`, every `signature` is plain UTF-8 — exactly
what v1 stores. When the optional `compress` Cargo feature is enabled and
the user has trained an FSST codec via `crabcc compress`, new rows get
written with `signature_enc = 1` and the byte content is FSST-encoded.
Readers branch on the column on the read path. Background and rationale
live in [RESEARCH-fsst.md](RESEARCH-fsst.md); see the Compression section
below for the short version.

## Indexing pipeline

`crabcc index` and `crabcc refresh` share the same path through
`crabcc-core`:

```
walker.rs       ignore-aware repo walk (gitignore + .ignore + hidden)
   │
   ▼
extract.rs      tree-sitter per language (TypeScript/TSX/JavaScript/Ruby)
   │            → Vec<Symbol>
   ▼
hash.rs         SHA-256 of file contents (skipped if unchanged on refresh)
   │
   ▼
store.rs        UPSERT files, REPLACE symbols, atomic transaction
   │
   ▼
fts.rs          Tantivy mirror (rebuilt on full index, NOT on refresh)
```

Properties:

- **Idempotent**: running `crabcc index` twice in a row yields a byte-stable
  database. `replace_symbols(file_id, …)` deletes and re-inserts rather
  than diffing — simpler and avoids stale rows on AST shape changes.
- **Incremental**: `crabcc refresh` reads `(path, sha256, mtime)` for every
  known file and only re-extracts the ones whose content hash changed.
- **Symbol-aware ignore**: walking respects `.gitignore`, `.ignore`, and
  hidden-file rules, matching `ripgrep`'s default behavior.
- **Tantivy lag**: by design, `refresh` does NOT rebuild Tantivy. Use
  `crabcc fts-rebuild` (or a fresh `crabcc index`) for fuzzy/prefix on
  newly-added symbols. The skill flags this so agents don't get confused
  by stale fuzzy hits.

## Query path

CLI:

```
clap parse → query.rs entrypoint → store.rs (rusqlite) → JSON or text
                                  └── fts.rs   (Tantivy) for fuzzy/prefix
```

MCP (`crabcc --mcp`):

```
stdio JSON-RPC frame → crabcc-mcp dispatch → query.rs (same entrypoints)
                                          → JSON-RPC response on stdout
```

Both surfaces hit the **same** `query.rs` functions (`find_symbol`,
`refs`, `outline`, …) so behavior, ranking, and limits are identical
between an agent calling the MCP and a human shelling out. The CLI just
adds presentation (`--json`, plain-text formatting, color).

`Mode::{Hits, FilesOnly, Count}` in `query.rs` controls how much we
materialize before the agent ever sees the result — that's where most of
the token-savings story lives. If a tool only needs file paths, we never
read line/col/snippet from SQLite.

## Storage choices

SQLite was chosen for symbol storage because exact `name` lookup, range
scans by `(file_id, line_start)`, and joins between `files`, `symbols`,
and `edges` are all things SQLite indexes natively. The `rusqlite` crate
is paired with WAL journal mode, a 30 GB `mmap_size` cap, `synchronous =
NORMAL`, and a 16 MB page cache — settings that keep concurrent
reader/writer access cheap during `crabcc watch`.

Tantivy 0.22 is layered on top because SQLite is bad at the queries the
CLI's UX leans on most heavily for human users:

- **Fuzzy** symbol search (Levenshtein distance ≤ 2) for typos.
- **Prefix** search for editor-style autocomplete.
- **Regex** search across the symbol-name corpus.

The tradeoff is two storage layers, but Tantivy is purely derivable from
SQLite, lives next to it (`.crabcc/tantivy/`), and rebuilds in seconds
even on large repos. Prior-art research in
[RESEARCH-mempalace.md](RESEARCH-mempalace.md) walks through why we
didn't pick LMDB / RocksDB / Tantivy-only / a custom file format.

## Compression (v2.0)

> Detailed plan: [RESEARCH-fsst.md](RESEARCH-fsst.md). Tracked in
> `.dev-tasks` under tags T01–T44.

Symbol signatures are highly redundant within a corpus — the bytes
`fn `, `(self, `, `pub async`, `Result<`, etc. repeat across thousands of
rows. We're integrating [FSST](https://github.com/spiraldb/fsst-rs) (Apache-2.0)
to train a per-repo symbol table, encode signature columns into a compact
byte stream, and decompress per row at query time.

Status: gated behind `crabcc-core`'s `compress` Cargo feature, off by
default until the four release-gate criteria in RESEARCH-fsst §6.2 are
met. The schema column `symbols.signature_enc` is already in v1 so the
migration to v2 is additive — old DBs keep working with `signature_enc
= 0` rows untouched.

## Performance

Three benchmark harnesses live under [`bench/`](../bench):

| Harness                                                     | Purpose                                                                              |
| ----------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| [`bench/raw-bench.py`](../bench/raw-bench.py)               | crabcc CLI vs raw `grep` / `find` / `cat` and `rg` / `fd` — bytes + ms per query.    |
| [`bench/codeindex-vs-crabcc.py`](../bench/codeindex-vs-crabcc.py) | crabcc vs the C++ `codeindex.cc` baseline (skips with exit 0 if codeindex absent). |
| `crates/crabcc-core/benches/symbols.rs`                     | Criterion micro-benches: `find_by_name` cold/warm, `iter_all_symbols`, `replace_symbols`. |

Run the criterion benches with `cargo bench -p crabcc-core`. Run the
Python benches with `python3 bench/<name>.py --repo <fixture>`. JSON
results land in `bench/results/`.

What we measure and why: the headline number is *bytes-out per query*
(token cost when an agent consumes the response) but *ms* matters too —
slow tools fall out of agent reach because Claude Code times out on
long-running shell commands. The criterion micro-benches keep an eye on
the SQLite hot paths so a regression there gets caught before it shows
up at the bench/CLI level.

## Extending: adding a new language

The whole tree-sitter integration funnels through five functions in
[`crates/crabcc-core/src/extract.rs`](../crates/crabcc-core/src/extract.rs).
To add (say) Python support, you'd touch each:

1. **`detect_lang`** — map the file extension(s) to a string tag
   (`"python"`, etc.).
2. **`extract_file`** — branch on the new tag to load the
   `tree_sitter_python::language()` parser. Add the parser crate to
   `crates/crabcc-core/Cargo.toml` and the workspace root.
3. **`symbol_kind_for`** — translate the parser's node kinds
   (`function_definition`, `class_definition`, …) into our `SymbolKind`
   enum.
4. **`signature_for`** — walk into the AST node and extract a compact
   one-line signature (parameter list, return type if present).
5. **`visibility_for`** — return `Some("pub")` / `Some("priv")` / `None`
   per the language's visibility rules. Most dynamic languages will
   return `None` here.

Then add a fixture file under `tests/fixtures/<lang>/` and an
integration test asserting the right symbols come back from
`Store::find_by_name` after indexing it. Tantivy / FTS picks up the new
kind automatically — no separate registration needed because Tantivy
mirrors whatever's in SQLite.

The same five-function pattern is followed for TypeScript, TSX,
JavaScript, and Ruby today. Anything that needs more — for instance,
parsing C++ overload sets or Rust trait impls — would warrant breaking
out a small `lang/<name>.rs` module rather than continuing to grow the
match arms in `extract.rs`.
