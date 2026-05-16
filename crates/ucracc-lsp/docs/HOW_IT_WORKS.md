# How `ucracc-lsp` works

A reference for **users** (editor setup, what each LSP method does, how
to talk to the custom commands) and **developers** (architecture, the
hot-path pipeline, extension points, public APIs).

Pair this with the [README](../README.md) for the elevator pitch +
install instructions; this document is the deeper dive.

---

## Table of contents

- [User guide](#user-guide)
  - [Languages](#languages)
  - [LSP methods we answer](#lsp-methods-we-answer)
  - [Custom commands (`workspace/executeCommand`)](#custom-commands-workspaceexecutecommand)
  - [Cargo features](#cargo-features)
  - [Tips and gotchas](#tips-and-gotchas)
- [Developer guide](#developer-guide)
  - [Architecture at a glance](#architecture-at-a-glance)
  - [The hot path: a `hover` request, end to end](#the-hot-path-a-hover-request-end-to-end)
  - [The write path: a `didChange` event, end to end](#the-write-path-a-didchange-event-end-to-end)
  - [Lazy loading](#lazy-loading)
  - [Incremental reparse](#incremental-reparse)
  - [The moka LRU cache](#the-moka-lru-cache)
  - [Extending the LSP](#extending-the-lsp)
  - [`crabcc-core` public API we depend on](#crabcc-core-public-api-we-depend-on)
  - [Testing patterns](#testing-patterns)
  - [Bench patterns](#bench-patterns)

---

## User guide

### Languages

Eleven languages, all backed by tree-sitter. The columns mark whether
documentSymbol / call-edge-driven features (`references`, `callHierarchy`)
are available.

| Language          | documentSymbol | references / callHierarchy | Parsed by |
|-------------------|:--:|:--:|---|
| Rust              | ✓ | ✓ | crabcc-core |
| TypeScript / TSX  | ✓ | ✓ | crabcc-core |
| JavaScript / JSX  | ✓ | ✓ | crabcc-core |
| Python            | ✓ | ✓ | crabcc-core |
| Ruby              | ✓ | ✓ | crabcc-core |
| Go                | ✓ | ✓ | crabcc-core |
| Swift             | ✓ | ✓ | crabcc-core (v0.2+) |
| Bash              | ✓ | ✓ | crabcc-core (v0.2+) |
| Java              | ✓ | ✓ | crabcc-core (v3.0.0-rc.4+) — class / interface / enum / record / method / constructor; method_invocation + object_creation_expression as call edges |
| YAML              | ✓ | — | ucracc-lsp (keys only) |
| Markdown          | ✓ | — | ucracc-lsp (heading outline) |

YAML and Markdown deliberately don't emit call edges — they're not code.

### LSP methods we answer

| LSP method | What you get | Backed by |
|---|---|---|
| `textDocument/documentSymbol` | The symbol outline of the file | `Store::symbols_in_file` |
| `textDocument/definition` | Jump to where a symbol is defined | `query::find_symbol` |
| `textDocument/references` | Find every reference to a symbol — single-file and cross-file | `query::find_refs ∪ query::find_callers` |
| `textDocument/hover` | Signature + file:line + parent for the identifier at the cursor | first symbol from `query::find_symbol` |
| `workspace/symbol` | Fuzzy / prefix search across the whole repo | tantivy `Fts::prefix` |
| `callHierarchy/prepare` | Pin a callable for the next two requests | `query::find_symbol` |
| `callHierarchy/incomingCalls` | Who calls this function? | `query::find_callers` |
| `callHierarchy/outgoingCalls` | What does this function call? | `graph::CallGraph::callees` |
| `workspace/executeCommand` | Custom commands (memory search, web fetch, rerank) | see below |
| `initialize` / `initialized` / `shutdown` | Standard handshake | n/a |
| `textDocument/didOpen|Change|Save|Close` | Indexes in-flight edits | this crate |

What we **don't** answer (intentionally — semantic LSPs own these):

- diagnostics
- completion
- code actions
- formatting / rename
- semantic tokens

Run ucracc-lsp **alongside** rust-analyzer / sourcekit-lsp / pyright / etc.
Most LSP clients merge results from multiple servers per filetype.

### Custom commands (`workspace/executeCommand`)

All custom commands are feature-gated. With no features enabled, only
the LSP nav surface exists.

| Command | Args | Returns | Feature |
|---|---|---|---|
| `ucracc.memory.search` | `[query: string, limit?: u64]` | Array of `Hit { drawer, score }` from `crabcc-memory`'s hybrid (BM25 ⊕ vector) search. If the `rerank` feature is also on, the top-50 results are reranked with bge-reranker-v2-m3 | `memory` |
| `ucracc.webfetch` | `[url: string]` | `FetchResult` from `crabcc-fetch` — cleaned main content + title for the URL | `fetch` |
| `ucracc.rerank` | `[query: string, docs: string[], top_n?: u64]` | `[{ index, score, document }]` — cross-encoder rerank with bge-reranker-v2-m3 | `rerank` |

Invocation example (LSP JSON-RPC):

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "workspace/executeCommand",
  "params": {
    "command": "ucracc.memory.search",
    "arguments": ["concurrency model", 5]
  }
}
```

Commands that aren't compiled in still answer — they return
`{"error":"ucracc-lsp built without `X` feature"}` so clients can detect
the gap without crashing.

### Cargo features

| Feature | Default | What it adds |
|---|:--:|---|
| `yaml` | on | tree-sitter-yaml extractor (top-level keys → DocumentSymbols) |
| `markdown` | on | tree-sitter-md extractor (heading outline) |
| `memory` | off | `ucracc.memory.search` command via `crabcc-memory` |
| `fetch` | off | `ucracc.webfetch` command via `crabcc-fetch` |
| `rerank` | off | `ucracc.rerank` command + auto-rerank inside `memory.search`. Lazy-downloads bge-reranker-v2-m3 (~1.1 GB) on first use into `~/.cache/crabcc-memory/` |

Swift and Bash used to be features here; in v0.2.0 they moved into
`crabcc-core` and are now always-on for every consumer of the workspace
(`crabcc index`, `crabcc sym`, the MCP server, this LSP).

### Tips and gotchas

- **`crabcc index` first.** ucracc-lsp expects `.crabcc/index.db` (and
  `.crabcc/tantivy/` for `workspace/symbol`) to exist. If they don't, the
  server starts in a "no-op" state — `documentSymbol` returns `None`,
  `workspace/symbol` returns empty. On launch, the server shows a warning
  message via `window/showMessage`.
- **`workspace/symbol` needs the tantivy sidecar.** `crabcc index` builds
  it; `crabcc refresh` does **not**. If you incrementally refresh a
  large repo and `workspace/symbol` feels stale, run `crabcc index`
  (or call `crabcc fts-rebuild`).
- **Cache invalidation on `didChange`.** The moka LRU is flushed on
  every write event. Stale hits are not a thing during editing. The
  cost: the first hover after a save pays the full 6–10 µs SQLite hit.
- **No diagnostics.** If you expected red squiggles, that's
  rust-analyzer / sourcekit-lsp / pyright's job. ucracc-lsp's value is
  *speed of navigation*, not type-checking.

---

## Developer guide

### Architecture at a glance

```
        ┌─────────────────────────────── client (editor) ───────────────────────────────┐
        │                                                                               │
        │   stdin / stdout (LSP JSON-RPC framing handled by tower_lsp::Server)          │
        │                                       │                                       │
        └───────────────────────────────────────┼───────────────────────────────────────┘
                                                ▼
                            ┌─────────────────────────────────────┐
                            │   Backend (impl LanguageServer)     │
                            │   - dispatch by method name         │
                            │   - moka LRU read-through cache     │
                            │   - per-doc Tree cache              │
                            └────────────────────┬────────────────┘
                                                 │
                            ┌────────────────────▼────────────────┐
                            │   State (RwLock + per-resource Mutexes)
                            │   - Store  (crabcc-core SQLite)     │
                            │   - Fts    (crabcc-core tantivy)    │
                            │   - CallGraph                       │
                            │   - HashMap<Url, String>  (text)    │
                            │   - HashMap<Url, Tree>    (parse)   │
                            └────────────────────┬────────────────┘
                                                 │
            ┌────────────────────────────────────┴───────────────────────────────────┐
            ▼                                                                        ▼
  ┌──────────────────────┐                                            ┌───────────────────────────┐
  │   crabcc-core         │                                            │  this crate (yaml.rs,     │
  │   - extract::*        │  ◀── tree-sitter grammars (Rust, TS,       │   markdown.rs)            │
  │   - query::*          │      JS, Python, Ruby, Go, Swift, Bash)    │   - tree-sitter-yaml      │
  │   - store::Store      │                                            │   - tree-sitter-md        │
  │   - fts::Fts          │                                            │                           │
  │   - graph::CallGraph  │                                            └───────────────────────────┘
  └──────────────────────┘
```

The crate is a **thin shell** around `crabcc-core`. We don't re-implement
parsing, symbol extraction, FTS, or call-graph walking — we adapt them
to LSP wire types and add LSP-specific concerns (per-doc state, lazy I/O,
INCREMENTAL sync, query caching).

### The hot path: a `hover` request, end to end

```text
client → JSON-RPC: textDocument/hover { uri, position }
   │
   │  (tower_lsp parses frame, calls Backend::hover)
   ▼
Backend::hover                                     —  src/server.rs
   1. acquire State (tokio RwLock read)
   2. word_at(state.open_docs, uri, position)      —  identifier under cursor
   3. cache.get(Hover(word))?                      —  on hit (~1.2 µs): return
   4. state.ensure_store()                         —  lazy SQLite open on first call
   5. crabcc_core::query::find_symbol(store, word) —  ~6 µs SQLite SELECT
   6. handlers::hover_for(&hits)                   —  format markdown
   7. cache.put(Hover(word), value)
   8. return Hover                                 —  serialized by tower_lsp
```

Total: **~7 µs cold**, **~1.2 µs warm**.

### The write path: a `didChange` event, end to end

```text
client → JSON-RPC: textDocument/didChange { uri, version, content_changes }
   │
   ▼
Backend::did_change                                —  src/server.rs
   1. Load current text mirror from state.open_docs
   2. For each TextDocumentContentChangeEvent:
        - if range is None: full replace, drop cached Tree
        - else: incremental::apply_change(text, ev)  →  text mutated in place,
          returns InputEdit describing the byte / row / column delta
   3. acquire RwLock write, replace open_docs entry, flush moka cache
   4. for each InputEdit: old_tree.edit(&edit)     —  threads the edits through
      the cached Tree so the next reparse can skip unchanged subtrees
   5. spawn_blocking Backend::index_uri(uri, src, root)
        ├─ ensure_store()
        ├─ Lang::from_path(...)
        │   └─ if handled_internally (YAML, Markdown): call yaml::extract / markdown::extract
        │   └─ else: drive tree-sitter ourselves
        │         a. parser.set_language(crabcc_core::extract::language(lang)?)
        │         b. old_tree = state.trees.lock().remove(uri)
        │         c. new_tree = parser.parse(src, old_tree.as_ref())   ←  incremental!
        │         d. extract_from_root(new_tree.root_node(), src, file, lang)
        │         e. store.upsert_file + replace_symbols + replace_edges
        │         f. state.trees.lock().insert(uri, new_tree)
        └─ done
```

The reparse step is the headline win: with the InputEdits applied to the
old tree, tree-sitter reuses subtrees outside the changed region. On a
~3 KLOC Rust file, a one-byte keystroke reparse drops from 1.32 ms (cold)
to 162 µs (8.2× faster).

### Lazy loading

`initialize` records paths only. `Store::open` and `Fts::open` are
deferred to first use via `State::ensure_store()` / `State::ensure_fts()`
which acquire the inner Mutex, check `is_some()`, and open on miss.

`initialized` (the LSP notification fired after the client receives our
initialize response) spawns a background `tokio::task::spawn_blocking`
that pre-warms both. By the time the user finishes hitting Cmd-S on
their first file, the SQLite + tantivy open is already done.

Result: `initialize` returns in ~24 µs (was 976 µs before lazy load).

### Incremental reparse

LSP sync is `TextDocumentSyncKind::INCREMENTAL` (set in
`Self::server_capabilities`). Each `didChange` event has a `range` that
describes a span of the previous text being replaced by `text`.

`incremental::apply_change(text, event) → Option<InputEdit>`:
- Converts the LSP `Range` (1-based line/column in UTF-16 by default)
  to UTF-8 byte offsets. We currently treat character offsets as UTF-8
  byte indices, which is correct for ASCII. Mixed-script files lose
  some incremental efficiency but stay correct (tree-sitter widens the
  reparse region if the edit boundary lands inside a UTF-8 codepoint).
- Mutates the in-memory `String` in place.
- Returns the `InputEdit` so the caller can apply it to the cached Tree.

The cached Tree lives in `State::trees: HashMap<Url, Tree>` behind a
sync Mutex. We never hold the Mutex across `.await`, which keeps the
data structure compatible with tokio's async runtime.

### The moka LRU cache

- Type: `moka::sync::Cache<Key, Arc<serde_json::Value>>`.
- Bounds: 1024 entries, 30 s TTL.
- Keys (`src/cache.rs::Key`):
  - `Definition(symbol_name)`
  - `Hover(symbol_name)`
  - `DocumentSymbols(rel_path)`
  - `WorkspaceSymbol { query, limit }`
  - `IncomingCalls(symbol_name)`
  - `OutgoingCalls(symbol_name)`
- Invalidated on every `didOpen` / `didChange` / `didSave`.

Hit shape: serde-deserialize the cached `Value` back into the LSP wire
type. The serialize/deserialize cost is real (~500 ns) but still beats
the SQLite hit by 5–500× depending on the operation. The biggest win is
`workspace/symbol` (tantivy + scoring at 604 µs → 1.1 µs cached, 550×).

### Extending the LSP

#### Add a new language already supported by crabcc-core

(e.g. Kotlin, C++, C#, if crabcc-core gets them in the future — Java
landed in v3.0.0-rc.4):

1. Add the variant to `Lang` in `src/lang.rs`.
2. Add the extension to `from_ext`.
3. Add the language ID to `SUPPORTED_LANGUAGE_IDS`.
4. Done — the dispatch in `Backend::index_uri` automatically delegates
   to `crabcc_core::extract::extract_from_root`.

#### Add a new language NOT in crabcc-core

Use YAML / Markdown as templates (`src/yaml.rs`, `src/markdown.rs`):

1. Add a Cargo feature in `Cargo.toml`, pull the tree-sitter crate as
   `optional = true`.
2. Add a new module under `src/`, gated on the feature.
3. Implement `pub fn extract(file, src) -> Result<(Vec<Symbol>, Vec<Edge>)>`.
   Use `crabcc-core`'s `Symbol` / `Edge` / `SymbolKind` types so the
   output flows into the same SQLite tables.
4. Add the language to `Lang::handled_internally` so the dispatch in
   `Backend::index_uri` routes it to your extractor.
5. Add a fixture in `tests/fixtures.rs` and a row in
   `document_symbol_covers_all_languages`.

If the language really is code (calls, references make sense), strongly
prefer adding it to `crabcc-core` so every consumer benefits — see
`extract.rs::detect_lang`, `ts_language`, `symbol_kind_for`, `is_callable`,
`call_target`, `visibility_for`. The Swift + Bash additions in v0.2.0 are
the reference implementations.

#### Add a new custom `executeCommand`

1. Add a constant in `src/commands.rs` (e.g. `pub const FOO: &str = "ucracc.foo"`).
2. Implement `pub fn foo(args: &[Value]) -> Result<Value>`.
3. Add it to `known_commands()` (so it appears in `ServerCapabilities`).
4. Add a match arm in `Backend::execute_command` in `src/server.rs`.
5. Gate behind a Cargo feature if it pulls a meaningful dep.

### `crabcc-core` public API we depend on

| Item | Module | What we use it for |
|---|---|---|
| `Store::open` | `store` | open SQLite, lazy-init on `ensure_store` |
| `Store::upsert_file` | `store` | record a parsed file |
| `Store::replace_symbols`, `replace_edges` | `store` | write extracted rows |
| `Store::symbols_in_file` | `store` | `documentSymbol` |
| `Fts::open`, `Fts::prefix` | `fts` | `workspace/symbol` prefix match |
| `query::find_symbol` | `query` | `definition`, `hover`, callHierarchy prepare |
| `query::find_refs` | `query` | `references` (JS/TS/Ruby coverage) |
| `query::find_callers` | `query` | `references` (everyone else), `incomingCalls` |
| `graph::CallGraph::build_from_edges` | `graph` | `outgoingCalls` |
| `extract::detect_lang` | `extract` | route paths to extractors |
| `extract::language` *(v0.2+)* | `extract` | get the tree_sitter::Language for our own Parser |
| `extract::extract_from_root` *(v0.2+)* | `extract` | walk a Tree we parsed |
| `extract::extract_file_with_edges` | `extract` | refresh-time fallback in didSave's spawn |
| `hash::sha256_hex` | `hash` | content-addressed dedup in `upsert_file` |
| `index::refresh` | `index` | sweep sibling edits on didSave |
| `types::{Symbol, Edge, SymbolKind, Hit}` | `types` | shared wire types |

### Testing patterns

Three layers, total 10 tests in the crate:

- **In-process integration tests** (`tests/integration_lsp.rs`). Drive
  the real `Backend` through the `LanguageServer` trait via `LspService::
  new(Backend::new).inner()`. Skips the JSON-RPC framing layer (that's
  tower-lsp's responsibility, not ours) but exercises every line of
  every handler.
- **Extractor correctness** (`tests/swift_extractor.rs`). Validates
  that the in-pipeline tree-sitter walker produces the right
  `Symbol` / `Edge` shape for a representative source file.
- **Shared fixtures** (`tests/fixtures.rs`). Tiny per-language sources;
  the integration test includes all of them.

To add a test:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn my_new_test() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_fixtures(&root);
    build_index(&root);   // crabcc_core::index::full_index + fts.rebuild

    let svc = boot(root.clone()).await;
    let uri = uri_for(&root, "ucracc.rs");
    open_doc(&svc, uri.clone(), "rust", fixtures::RUST_SRC).await;

    let resp = svc.inner().my_method(MyParams { ... }).await.expect("call");
    // assert on resp
}
```

The `boot()` and `open_doc()` helpers in `tests/integration_lsp.rs`
handle the LSP handshake + didOpen so test bodies stay focused.

### Bench patterns

Two criterion bench targets:

- `benches/baseline_vs_lsp.rs` — baseline (calling `crabcc-core`
  directly) vs the LSP wrapper, cold vs cached. Where to look for
  per-request hot-path numbers.
- `benches/extractor_cost.rs` — per-language parse vs parse+walk; and
  the full-vs-incremental reparse comparison on a 100-fn Rust file with
  a one-byte edit.

Both use `Criterion::default().sample_size(50)`. Run quick mode with
`cargo bench -p ucracc-lsp -- --quick` for ~10 s feedback loops; drop
`--quick` for the full distribution.

To add a benchmark:

```rust
fn bench_my_thing(c: &mut Criterion) {
    let bed = setup();  // shared Bed: tempdir, store, runtime, service
    c.bench_function("my_thing", |b| {
        b.iter(|| {
            bed.rt.block_on(async {
                let r = bed.service.inner().my_method(...).await.unwrap();
                black_box(r);
            });
        });
    });
}
```

Add to `criterion_group!` targets at the bottom of the file. Don't
forget to drop `--default-features` if your bench needs a non-default
feature on, or to gate the bench function on `#[cfg(feature = "...")]`.
