# How `crabcc-core` works

A reference for **library consumers** (the `crabcc` CLI, the MCP server,
`crabcc-viz`, `ucracc-lsp`, anything else linking against this crate)
and **developers** working inside `crabcc-core` itself.

This is the *engine room* documentation — what the index actually
stores, how the pipeline routes a source file to symbol rows, where the
public API lines are drawn. For the user-facing CLI surface see the
crabcc README; for the navigation LSP that consumes us see
[`crates/ucracc-lsp/docs/HOW_IT_WORKS.md`](../../ucracc-lsp/docs/HOW_IT_WORKS.md).

---

## Table of contents

- [Library guide](#library-guide)
  - [What lives where (on disk)](#what-lives-where-on-disk)
  - [The data model](#the-data-model)
  - [Public modules at a glance](#public-modules-at-a-glance)
  - [Cargo features](#cargo-features)
  - [Minimal usage example](#minimal-usage-example)
- [Internals](#internals)
  - [Indexing pipeline](#indexing-pipeline)
  - [Query pipeline](#query-pipeline)
  - [SQLite schema](#sqlite-schema)
  - [Tree-sitter integration](#tree-sitter-integration)
  - [Edges and the call graph](#edges-and-the-call-graph)
  - [Native fuzzy/prefix search (FTS)](#native-fuzzyprefix-search-fts)
  - [FSST signature compression](#fsst-signature-compression)
  - [Watch / refresh / refresh_delta](#watch--refresh--refresh_delta)
- [Extending `crabcc-core`](#extending-crabcc-core)
  - [Add a new language](#add-a-new-language)
  - [Add a new symbol kind](#add-a-new-symbol-kind)
  - [Evolve the schema](#evolve-the-schema)
- [Testing patterns](#testing-patterns)

---

## Library guide

`crabcc-core` is the indexing, storage, and query layer behind the
`crabcc` CLI, the MCP server, and `ucracc-lsp`. It does **one job**:
turn source files in a directory tree into a queryable symbol /
edge / FTS index, persistently, fast, and additively (no destructive
schema migrations).

It does **not** speak HTTP, LSP, MCP, or any wire protocol. Those are
the caller's job — `crabcc-core` is library-shaped on purpose so the
same engine powers a CLI, an LSP, an HTTP dashboard, and any future
front-end without forking.

### What lives where (on disk)

Per-repo state goes under `<repo>/.crabcc/`:

| Path | Role | Built by | Rebuilt by |
|---|---|---|---|
| `index.db` | SQLite store: `files`, `symbols`, `edges`, `meta` | `index::full_index` / `refresh` | `Store::open` runs idempotent migrations |
| `graph.json` | Call-graph sidecar — built from `edges`, mmap-friendly | `graph::CallGraph::build_from_edges` + `save` | on demand |
| `fsst.symbols` | Optional FSST codec table for signature compression | `compress::train` (offline) | one-time training; reused thereafter |
| `track.json` | Token-savings telemetry per CLI call | `track::record` | append-only |

### The data model

Four tables. The shape is intentionally *flat and unresolved* — names
are strings, not foreign keys to other symbols. Resolution is the
caller's job (the CLI's `find_symbol` does an exact-name SELECT;
`ucracc-lsp`'s `definition` handler does the same).

| Table | Purpose |
|---|---|
| `files` | One row per indexed source file. `(path UNIQUE, sha256, mtime, lang, indexed_at)` |
| `symbols` | One row per declaration: `(file_id, name, kind, signature, parent, line_start, line_end, visibility, signature_enc)` |
| `edges` | One row per call site (or import / inherit / ref): `(src_file_id, src_symbol, dst_name, kind, line)`. `src_symbol` is the *name* of the enclosing fn (null if file-level). |
| `meta` | Key-value scratchpad: `schema_version`, `edges_populated`, etc. |

Indexes on `symbols(name)`, `symbols(file_id)`, `symbols(name, kind)`,
`edges(dst_name)`, `edges(dst_name, kind)`. The composite indexes are
what make `query::find_callers` and the outline path constant-time.

Wire types live in `crate::types`:

```rust
pub enum SymbolKind {
    Function, Method, Class, Struct, Enum, Trait,
    Interface, Const, Var, Type, Macro,
}

pub struct Symbol { name, kind, signature: Option<String>, parent: Option<String>,
                    file, line_start: u32, line_end: u32, visibility: Option<String> }

pub struct Edge   { src_file, src_symbol: Option<String>, dst_name, kind, line: u32 }

pub struct Hit    { file, line: u32, col: u32, snippet }    // for refs/grep results
```

All four are `serde::{Serialize, Deserialize}` — that's the contract
the MCP server, the CLI's `--json`, and `ucracc-lsp`'s
`workspace/executeCommand` returns all rely on.

### Public modules at a glance

(Authoritative list — every `pub mod` in `src/lib.rs`.)

| Module | What it owns | Used by |
|---|---|---|
| `compress` | FSST codec — train / encode / decode signatures. Feature-gated on `compress`. | `store` for signature roundtrips |
| `config` | Disk paths + repo-root resolution helpers | CLI, every consumer |
| `extract` | tree-sitter parsers, per-language walkers, symbol + edge extraction. **New public API in v0.2:** `language()`, `extract_from_root()`. | `index`, `ucracc-lsp` |
| `fts` | Native fuzzy (Levenshtein 2, token-aware) + prefix lookup, built in-memory from the live symbol table | `crabcc fuzzy`, `crabcc prefix`, `ucracc-lsp::workspace/symbol` |
| `gitdiff` | `git diff --name-only SHA1..SHA2` → file set, for `--since SHA` filters | `query` |
| `graph` | `CallGraph` — build / save / walk / cycles / orphans | `crabcc graph *`, `ucracc-lsp::outgoingCalls` |
| `hash` | `sha256_hex`, content-addressed envelope fingerprint | `store::upsert_file`, dedup |
| `index` | `full_index`, `refresh`, `refresh_delta` — the two indexer entrypoints | CLI, LSP didSave |
| `jobs` | BullMQ-backed jobs API. Feature-gated on `jobs`. | (future) `crabcc jobs` |
| `md` | CommonMark parser glue. Feature-gated on `markdown`. | (future) drawer sanitizer |
| `ollama_stack` | Docker-Compose helper for the local Ollama stack | CLI subcommands |
| `outline` | File-scoped top-symbol list — no bodies | CLI, LSP `documentSymbol` |
| `pattern` | ast-grep per-language patterns + `lang_for` resolver | `query::query_callers` fallback (pre-edges) |
| `query` | `find_symbol`, `find_refs`, `find_callers`, plus the shaping `Mode` enum and structured `Output` | CLI, LSP nav handlers |
| `refs` | Streaming ref-walker (tree-sitter identifier scan; JS/TS/Ruby only) | `query::query_refs` |
| `service_discovery` | Find / health-check sidecar services (Redis, etc.) | `crabcc jobs` |
| `store` | Schema bootstrap, migrations, CRUD on `files` / `symbols` / `edges` / `meta` | every read + write path |
| `track` | Token-savings telemetry — `record`, `report`, `tokens_for_bytes` | CLI for `crabcc track` |
| `types` | Public data types: `Symbol`, `Edge`, `Hit`, `SymbolKind` | everyone |
| `upgrade` | Compare local crate version to latest GitHub release | `crabcc upgrade` |
| `walker` | `walk_repo(root)` — gitignore-aware directory iterator | `index::*` |
| `watch` | `notify-debouncer-mini`-backed file-watcher hook | `crabcc watch` |

Top-level re-exports (`crate::{Edge, Hit, Symbol, SymbolKind}`) cover
the wire types so consumers don't have to write `crate::types::Symbol`.

### Cargo features

| Feature | Default | What it adds |
|---|:--:|---|
| `compress` | on | FSST codec for `symbols.signature_enc`. Pulls `fsst-rs`. Compressed signatures decode transparently on read. Disable to drop the dep; on-disk encoded rows stay correct but return `None` until re-enabled. |
| `markdown` | off | `pub mod md` — wooorm/markdown-rs glue for CommonMark parsing. Used by future drawer sanitizers and `crabcc docs`. |
| `jobs` | off | `pub mod jobs` — BullMQ-backed job runner. Pulls `tokio` + `redis`. Default OFF because the rest of the surface stays sync. |
| `bench` | off | Gates `criterion` bench targets so `cargo build --all-targets` doesn't compile them. Run benches with `cargo bench --features bench`. |
| `testcontainers` | off | Gates the testcontainers-rs e2e suite for `service_discovery`. Needs Docker / OrbStack on the host. |

### Minimal usage example

```rust
use crabcc_core::{index, query, store::Store};
use std::path::Path;

let root = Path::new(".");
let db   = root.join(".crabcc/index.db");
std::fs::create_dir_all(db.parent().unwrap())?;

// Open (creates + migrates if missing).
let store = Store::open(&db)?;

// Index every supported file under `root`. Idempotent — re-running is a
// no-op except for files whose mtime/sha changed.
let stats = index::full_index(root, &store)?;
eprintln!("indexed {} files", stats.indexed);

// Query: every definition of `Foo`.
for sym in query::find_symbol(&store, "Foo")? {
    println!("{} @ {}:{}", sym.name, sym.file, sym.line_start);
}

// Query: every caller of `Foo`.
for hit in query::find_callers(&store, root, "Foo")? {
    println!("{}:{} {}", hit.file, hit.line, hit.snippet);
}
```

The `Store` is `Send` but **not** `Sync`. For multi-threaded readers,
wrap it in `Mutex<Store>` (what `crabcc watch` and `ucracc-lsp` both
do). The underlying SQLite is in WAL mode so readers don't block
writers even within the single process.

---

## Internals

### Indexing pipeline

`index::full_index(root, &store)` is the workhorse. The shape:

```text
walker::walk_repo(root)          ─▶  iterates files, gitignore-aware,
                                     skips hidden / build / vendored
                                     trees
        │
        ▼
extract::detect_lang(path)       ─▶  returns "rust" | "typescript" | …
                                     or None (skip)
        │
        ▼
fs::read(file)                   ─▶  bytes + mtime
        │
        ▼
hash::sha256_hex(bytes)          ─▶  content fingerprint
        │
        ▼
extract::extract_file_with_edges ─▶  (Vec<Symbol>, Vec<Edge>)
        │
        ▼
Store::upsert_file               ─▶  files row, returns file_id
Store::replace_symbols           ─▶  delete prior + bulk-insert (txn)
Store::replace_edges             ─▶  same
```

After the loop, `meta('edges_populated') = '1'` is set so
`query_callers` can take the edges-fast-path instead of the legacy
pattern walker.

`refresh(root, &store)` is the incremental variant: read each file's
mtime, skip if `(path, mtime)` matches the stored value, hash if it
doesn't, and only re-extract on a real content change.
`refresh_delta` does the same and *also* returns the per-bucket file
lists (added / modified / removed) — useful for consumers that want to
re-read only what changed (`ucracc-lsp` doesn't currently use this but
plausibly will when we add incremental FTS).

### Query pipeline

Two entry points, both in `query`:

1. **Edges-fast-path**. After v2.0, the `edges` table has every call
   site indexed by `dst_name`. `query::find_callers(store, root, name)`
   reduces to a single `SELECT line, file, snippet FROM edges JOIN
   files USING (file_id) WHERE dst_name = ?` — sub-millisecond on any
   reasonable index.

2. **Pattern walker fallback** (`query::run` + per-language closures in
   `pattern.rs`). Used when `meta('edges_populated') != '1'` (pre-v2
   DBs) or when the caller wants a streaming grep-shape (`refs`). The
   walker visits every file in the store, memchr-prefilters for the
   needle, and only invokes the tree-sitter / ast-grep walker on real
   hits.

The `Mode` enum (`Hits`, `Count`, `FilesOnly`, `Summary`) shapes the
output. The CLI exposes all four (`--count`, `--files-only`, etc.) so
agents can request the smallest payload that answers their question.
`ucracc-lsp` only uses `Mode::default()` (Hits).

### SQLite schema

Source of truth: [`schema/001_init.sql`](../../../schema/001_init.sql).
The whole script runs on every `Store::open` via SQLite's `CREATE TABLE
IF NOT EXISTS` semantics, so it's idempotent. In-place migrations
(adding `signature_enc`, retyping `edges.src_symbol` from INTEGER to
TEXT) live in `store::open_with_compress` after the script runs.

**Schema discipline (load-bearing):**

- Schema changes are **additive only**. Never `DROP COLUMN`. Never
  rename a column. Add a new column with `ALTER TABLE … ADD COLUMN`
  guarded by a `pragma_table_info` probe; old DBs upgrade in place,
  new DBs see the column declared up-front. The two prior migrations
  (FSST `signature_enc`, edges `src_symbol` retype) are the templates.
- `meta('schema_version')` exists for forensics but we don't gate
  reads on it — the migration probes are the contract.

**Pragmas set at open time:**

| Pragma | Value | Why |
|---|---|---|
| `journal_mode` | `WAL` | concurrent readers + faster writes |
| `synchronous` | `NORMAL` | fast but durable on power loss |
| `foreign_keys` | `ON` | makes `ON DELETE CASCADE` fire on `delete_file` |
| `mmap_size` | 30 GB | upper bound on memory-mapped I/O (caps to file size) |
| `temp_store` | MEMORY | keep `ANALYZE` / sort spills in RAM |
| `cache_size` | -64000 (64 MB) | sized to hold the hot index for ~13k-file repos |
| `busy_timeout` | 2 s | absorbs lock contention during `crabcc watch` refreshes |

### Tree-sitter integration

`extract.rs` is the per-language pipeline. Two layers:

**Layer 1 — language plumbing:**

- `detect_lang(path: &Path) -> Option<&'static str>` — extension → lang tag.
- `language(lang: &str) -> Result<tree_sitter::Language>` *(v0.2+ public)* — lang tag → tree-sitter Language. Used by LSP-style consumers that drive their own Parser for incremental reparse.
- `ts_language(lang)` — same as above, private alias kept for in-module use.

**Layer 2 — extraction:**

- `extract_file(file, src, lang) -> Vec<Symbol>` — symbols only.
- `extract_file_with_edges(file, src, lang) -> (Vec<Symbol>, Vec<Edge>)` — symbols + call edges. Single parse; the indexer uses this.
- `extract_from_root(root, src, file, lang) -> (Vec<Symbol>, Vec<Edge>)` *(v0.2+ public)* — walks an already-parsed Tree. Lets callers that own the Parser (LSP didChange path) reuse a tree they parsed incrementally.

**Per-language dispatch tables** (all keyed on the same `lang: &str`):

- `symbol_kind_for(lang, ts_kind) -> Option<SymbolKind>` — which tree-sitter node kinds become which `SymbolKind`. The decision tree per language.
- `is_callable(lang, ts_kind) -> bool` — does this node introduce a new enclosing scope for call edges? (Functions, methods, init/deinit, etc.)
- `call_target(node, src, lang) -> Option<(name, line)>` — given a call-expression-shaped node, what's the callee name?
- `visibility_for(lang, node, src) -> Option<String>` — `pub` / `priv` / etc. Lang-specific (Go does it by capitalization, Python by leading underscore, Rust by visibility_modifier child).

The generic recursive walker (`walk`, `walk_edges`) descends the parse
tree once and consults the four dispatch tables. Adding a language is
"new arm in each table" — see the [extension recipe](#add-a-new-language).

**Parser pool** *(v0.2+)*: `extract.rs` keeps one `tree_sitter::Parser`
per thread per language in a `thread_local!` `HashMap<&'static str,
Parser>`. Constructing a Parser + calling `set_language` is ~5–10 µs of
overhead per file; the pool collapses that to once per thread per lang.

### Edges and the call graph

`extract::extract_file_with_edges` populates `Vec<Edge>` during the
same tree walk that produces symbols. Each `call_expression` node (or
language-equivalent — Ruby `call`, Bash `command`, Swift
`call_expression`) gets one row:

```
Edge { src_file, src_symbol: enclosing_fn, dst_name: callee, kind: "call", line }
```

Resolution is deliberately *unresolved*. `dst_name` is the bare
identifier; for `obj.foo()` we record `foo`. Multiple `foo`s in the
repo all show up in `find_callers("foo")` — let the caller filter.
This matches how agents actually use the index (grep-shaped, name-
keyed) and avoids the AST-vs-AST resolution rabbit hole.

`graph::CallGraph` is a derived view, not authoritative storage:

```rust
pub struct CallGraph {
    pub callees: BTreeMap<String, BTreeSet<String>>,  // src_symbol → callees
    pub callers: BTreeMap<String, BTreeSet<String>>,  // callee     → callers
}
```

`build_from_edges(store)` runs one SQL scan over `edges` to materialize
both maps. `walk(name, dir, depth)` is BFS over either map.
`cycles()` is Tarjan's SCC; `orphans()` is "in `callees` but absent from
`callers`". The struct serializes to `graph.json` for sharing /
diffing.

### Native fuzzy/prefix search (FTS)

`fts::Fts` is an in-memory view of the symbol names, built straight from the
live SQLite store via `Fts::from_store(&store)` (which uses a name-only
projection, `Store::iter_symbol_names`, so the FSST-compressed `signature`
column is never decoded). Each `Row` precomputes the lowercased name and its
alphanumeric **tokens**, so matching works *within* a `snake_case`/dotted name —
matching what the former tokenized Tantivy index did. Replaced the Tantivy
sidecar in v6.2.0 ([#700](https://github.com/crabcc-labs/crabcc/pull/700)),
which removed ~20 transitive crates from the build.

Two query shapes:

- `fuzzy(query, limit)` — bounded Levenshtein (distance ≤ 2) against the whole
  name or any token (`strore` → `store`; `usr` → `get_user_profile`). The DP is
  allocation-free over bytes (ASCII fast path) with a reusable scratch, and
  fast-bails once it has `limit` exact hits or a full candidate pool — so a
  query matching most of the corpus returns in ~µs ([#713](https://github.com/crabcc-labs/crabcc/pull/713)).
- `prefix(query, limit)` — case-insensitive starts-with on the whole name or any
  token, shortest match first. For `workspace/symbol` autocompletion.

`score` is a synthetic closeness rank (`1.0` exact, `0.5` at distance 1, `0.33`
at distance 2), not BM25. Because the view is rebuilt from the live store on each
call (and cached + invalidated-on-edit in the LSP), **results always reflect the
current index — there is no sidecar and no staleness window.** The `crabcc
fts-rebuild` CLI command is retained as a no-op (reports the symbol count) for
backward compatibility.

### FSST signature compression

When the `compress` feature is built in (default ON) and a
`fsst.symbols` codec file exists alongside `index.db`, `Store::open`
loads the codec and:

- **On write**: encode each symbol's `signature` and set
  `signature_enc = 1`. Mixed encoded + plain rows coexist (rows
  inserted before the codec existed stay plain).
- **On read**: detect `signature_enc = 1`, decode through the codec.
  Transparent to all consumers — `Symbol.signature` is still
  `Option<String>` on the wire.

Compression ratio on real repos: ~3-5× on signatures (codebases share
common prefixes / parameter shapes). The codec itself is ~20 KB. Train
once per repo with `compress::train` (offline; the CLI has a
subcommand).

Disable with `--no-default-features` (drops `fsst-rs` from the build),
or at runtime via `Store::open_with_compress(path, false)` — encoded
rows on disk stay correct but return `None` until the codec is
re-enabled.

### Watch / refresh / refresh_delta

`watch::watch(root, &store, callback)` wraps
`notify-debouncer-mini`. Events arrive coalesced (50 ms debounce);
each event becomes a `refresh_delta` call against the affected file.
The callback receives the delta so consumers can react (LSP
invalidation, dashboard pushes).

`refresh_delta` is the "report what changed" entry; `refresh` is the
"just apply it" variant. Both share the same hot loop in `index.rs`.

---

## Extending `crabcc-core`

### Add a new language

If a tree-sitter grammar crate exists for it (`tree-sitter-FOO` on
crates.io, compatible with our `tree-sitter = "0.26"` workspace pin):

1. **Workspace `Cargo.toml`**: add `tree-sitter-foo = "X.Y"` under
   `[workspace.dependencies]`.
2. **`crates/crabcc-core/Cargo.toml`**: add `tree-sitter-foo = {
   workspace = true }` under `[dependencies]`.
3. **`src/extract.rs`** — four new arms:
    - `detect_lang`: extension → `"foo"` lang tag.
    - `ts_language`: `"foo"` → `tree_sitter_foo::LANGUAGE.into()`.
    - `symbol_kind_for`: per-`SymbolKind` mapping of tree-sitter node
      kinds.
    - `is_callable`: which nodes introduce a new enclosing scope.
    - `call_target`: how to extract callee names from this lang's
      call-expression shape.
    - `visibility_for`: lang-specific visibility heuristic (or `None`).
4. **`src/extract.rs::intern_lang`** *(v0.2+)*: add the new lang tag to
   the parser-pool lookup so it picks up the cached Parser.
5. **Special-case `node_name` if needed**: e.g. Swift's `init` /
   `deinit` decls don't have a `name` field — synthesize the keyword.
6. **Add a `#[test]`**: parse a tiny fixture, assert the right symbols
   + edges come out. The existing `extract::tests::*` are the template.

If the grammar isn't compatible with `tree-sitter = "0.26"` (older ABI
or a `links = "tree-sitter"` conflict): host the extractor in a
*consumer* crate (like `ucracc-lsp` does for YAML / Markdown), feed
results back into the same `Store::upsert_file + replace_symbols +
replace_edges` API.

The Swift + Bash additions in v0.2.0 are the reference implementation
for the "code, fits crabcc-core's mission" path. YAML + Markdown in
`ucracc-lsp` are the reference for the "data, doesn't fit the mission"
path.

### Add a new symbol kind

`SymbolKind` is the wire enum (`crate::types`). Adding a variant is a
breaking-ish change to all consumers because they pattern-match on
it. Process:

1. Add the variant.
2. Add it to `serde rename_all = "snake_case"` — write a test
   asserting the JSON shape (the existing `symbol_kind_*` tests cover
   this).
3. Map it in `Store::kind_str` / `Store::kind_from_str`.
4. Plumb it through every `symbol_kind_for` arm that should now emit
   it.
5. Bump the CHANGELOG and call out the wire change.

### Evolve the schema

**Only additive changes.** The recipe:

1. Add the column to `schema/001_init.sql` — old DBs ignore it because
   `Store::open` runs the script with `IF NOT EXISTS`.
2. Add an in-place migration in `store::open_with_compress`: probe
   with `PRAGMA table_info`, `ALTER TABLE ... ADD COLUMN` on miss.
3. Bump `meta.schema_version` (forensic-only, no gating).
4. Add a test in `store::tests` that opens a pre-migration DB and
   asserts the new column is present + the migration is idempotent.
5. If the column needs to be set by every writer: handle it in
   `Store::upsert_file` / `replace_symbols`. If it's a derived /
   optional column, leave existing rows alone.

Examples in tree: `signature_enc` (FSST flag) and the v1→v2
`edges.src_symbol` retype both followed this pattern.

---

## Testing patterns

`crabcc-core` has **230+ lib tests** + 11 ignored (FS-event tests that
are racy on CI; run locally with `cargo test -- --ignored`). The
patterns are stable across modules — copy any of them.

### Pattern: schema test

Open an in-memory or tempdir Store, run the action, query state with
the `Store`'s public API (not raw SQL — that's a refactor hazard).

```rust
#[test]
fn upsert_file_returns_stable_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("idx.db")).unwrap();
    let a = store.upsert_file("a.ts", "h1", 0, "typescript").unwrap();
    let b = store.upsert_file("a.ts", "h2", 1, "typescript").unwrap();
    assert_eq!(a, b, "upsert on same path must return the same row id");
}
```

### Pattern: extractor test

Build a string fixture, call `extract_file_with_edges`, assert on the
returned `Vec<Symbol>` / `Vec<Edge>`. No DB needed.

```rust
#[test]
fn extracts_rust_methods_with_impl_parent() {
    let src = r#"struct Foo;
impl Foo { fn bar(&self) {} }"#;
    let (syms, _) = extract_file_with_edges("a.rs", src, "rust").unwrap();
    let bar = syms.iter().find(|s| s.name == "bar").unwrap();
    assert_eq!(bar.kind, SymbolKind::Method);
    assert_eq!(bar.parent.as_deref(), Some("Foo"));
}
```

### Pattern: end-to-end via `full_index`

For tests that exercise the indexer + querier together, write a
tempdir worth of fixtures, run `full_index`, query, assert.

```rust
let tmp = tempfile::tempdir().unwrap();
std::fs::write(tmp.path().join("a.rs"), "pub fn hello() {}").unwrap();
let store = Store::open(&tmp.path().join(".crabcc/index.db")).unwrap();
let _ = index::full_index(tmp.path(), &store).unwrap();
assert_eq!(query::find_symbol(&store, "hello").unwrap().len(), 1);
```

This is the same shape `ucracc-lsp`'s integration tests use — the
helpers there (`write_fixtures`, `build_index`) are a thin wrapper.

### Run the suite

```bash
cargo test -p crabcc-core --lib                # 230 tests, ~1.2 s on M-series
cargo test -p crabcc-core --features bench     # benches compile (don't run them as tests)
cargo test --features testcontainers \
           -p crabcc-core --test service_discovery_e2e   # needs Docker
```

Cross-cutting: `cargo test --workspace --lib --tests` runs everything
in this repo. Last measured **716 tests / 0 failures** as of v0.2.0.
