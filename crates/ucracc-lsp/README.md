# `ucracc-lsp`

A **navigation + retrieval** Language Server Protocol server backed by
the `crabcc` symbol DB. Coexists with semantic LSPs (rust-analyzer,
sourcekit-lsp, pyright, tsserver) — does **not** replace them. Optimised
for instant cold start and sub-10 µs hot-path lookups.

## What it does

Per file type — Rust, TypeScript / TSX, JavaScript / JSX, Python,
Swift — ucracc-lsp answers:

| LSP method                       | Backed by                                    |
|----------------------------------|----------------------------------------------|
| `textDocument/documentSymbol`    | `Store::symbols_in_file`                     |
| `textDocument/definition`        | `query::find_symbol`                         |
| `textDocument/references`        | `query::find_refs` ∪ `query::find_callers`   |
| `textDocument/hover`             | symbol record (signature + file:line)        |
| `workspace/symbol`               | tantivy `Fts::prefix`                        |
| `callHierarchy/{prepare,in,out}` | `CallGraph::build_from_edges`                |
| `workspace/executeCommand`       | `ucracc.memory.search`, `.webfetch`, `.rerank` (feature-gated) |

What it does **not** do: diagnostics, completion, code actions, formatting,
rename. Those need a type system; rust-analyzer / sourcekit-lsp / etc.
own that turf. Run ucracc-lsp **alongside** them — your LSP client will
merge results.

## Language coverage

| Language        | Symbols | Edges | Source                                |
|-----------------|--------|-------|----------------------------------------|
| Rust            | ✓      | ✓     | crabcc-core extractor (tree-sitter)    |
| TypeScript/TSX  | ✓      | ✓     | crabcc-core extractor                  |
| JavaScript/JSX  | ✓      | ✓     | crabcc-core extractor                  |
| Python          | ✓      | ✓     | crabcc-core extractor                  |
| Ruby            | ✓      | ✓     | crabcc-core extractor                  |
| Go              | ✓      | ✓     | crabcc-core extractor                  |
| Swift           | ✓      | ✓     | this crate (tree-sitter-swift)         |

## Features

| Cargo feature | Default | What it adds                                                     |
|---------------|---------|------------------------------------------------------------------|
| `swift`       | on      | tree-sitter-swift extractor                                      |
| `memory`      | off     | `ucracc.memory.search` (BM25 ⊕ vector ⊕ optional rerank)         |
| `fetch`       | off     | `ucracc.webfetch` via `crabcc-fetch`                              |
| `rerank`      | off     | `ucracc.rerank` + auto-rerank for memory.search (bge-reranker-v2-m3) |

The reranker lazy-downloads a ~1.1 GB ONNX model on first use into
`~/.cache/crabcc-memory/`. Rerank is capped to the top-50 fusion
candidates; beyond that the cross-encoder stops paying for itself.

## Install / wire up

```bash
# build
cargo build --release -p ucracc-lsp

# the binary lives at target/release/ucracc-lsp; it speaks LSP over stdio
```

Editor config — Neovim example (alongside rust-analyzer):

```lua
require("lspconfig.configs").ucracc_lsp = {
  default_config = {
    cmd = { "/path/to/ucracc-lsp" },
    filetypes = { "rust", "typescript", "typescriptreact",
                  "javascript", "javascriptreact", "python", "swift" },
    root_dir = require("lspconfig.util").root_pattern(".crabcc", ".git"),
  },
}
require("lspconfig").ucracc_lsp.setup({})
```

ucracc-lsp expects `.crabcc/index.db` (and `.crabcc/tantivy/` for
workspace symbol) to exist. Run `crabcc index` in the repo once.

## Performance

`cargo bench -p ucracc-lsp --bench baseline_vs_lsp` — measured on M-series
Apple silicon, dev fixtures (single Rust file, ~10 symbols, 1 call edge).
**Baseline** = calling `crabcc-core` directly. **LSP cold** = first call
after cache flush. **LSP cached** = repeat of the same call in the same
edit-session.

| Operation             | Baseline | LSP cold | LSP cached | Cache win |
|-----------------------|----------|----------|------------|-----------|
| cold start            | 720 µs   | 1.05 ms  | n/a        | n/a       |
| `documentSymbol`      | 8.2 µs   | 8.7 µs   | 3.7 µs     | 2.4×      |
| `definition`          | 6.2 µs   | 7.2 µs   | 1.1 µs     | 6.5×      |
| `hover`               | 6.2 µs   | 6.9 µs   | 1.2 µs     | 5.7×      |
| `workspace/symbol`    | 601 µs   | 604 µs   | 1.1 µs     | **550×**  |

The dispatch wrapper adds < 1 µs per hot-path call (URL parse + Mutex
acquire + Tokio task hop). Cold start is dominated by tantivy mmap
setup. The cache is a moka LRU (1024 entries, 30 s TTL) flushed on
every `didOpen` / `didChange` / `didSave`, so we never return stale
results.

### Hard targets (all met)
- Cold start to `initialized`: **< 100 ms**
- hover / definition / references on warm index: **< 20 ms p95**
- `workspace/symbol` (200-char query, top 20): **< 30 ms p95**
- Rerank pass over 50 candidates: **< 50 ms p95** on M-series

## Tests

```bash
cargo test -p ucracc-lsp --tests
```

- `tests/integration_lsp.rs` — drives the real `Backend` through
  `LanguageServer` trait methods. Covers:
  - documentSymbol across 6 languages (Rust, TS, Python, Ruby, Go, Swift),
  - definition, hover, workspace/symbol,
  - references **single-file** and **cross-file**,
  - **cache invalidation on `didChange`** (rename round-trip),
  - **callHierarchy** prepare + incomingCalls.

  No subprocess, no JSON-RPC framing layer (that's tower-lsp's
  responsibility, not ours).
- `tests/swift_extractor.rs` — correctness check for the Swift
  tree-sitter walker: class, init, free fn, parent linkage, call edges.

Unit-test surface is intentionally tiny — the integration tests cover
every public path end-to-end.

## Future ideas (deferred)

These are bookmarked but **not** in v1:

- **jemalloc on Linux release builds** (`tikv-jemallocator`). Lower
  allocator overhead in tantivy + serde_json hot paths. Cheap to add
  behind a Cargo feature.
- **Linking with [Wild](https://github.com/davidlattimore/wild)** on
  Linux. Shaves dev-loop link time once it's stable enough for our
  build matrix.
- **Cross-process FTS preview cache.** `workspace/symbol`'s 604 µs is
  almost entirely tantivy. A small LRU on (query, top-K) would cut p99
  on repeated typing.
- **Reuse the parsed-tree across `didChange` events.** tree-sitter
  supports incremental reparse — we currently throw the tree away. Would
  help large files (>10 KLOC) but isn't measurable on the bench fixtures.

Lattimore's "Wild performance tricks" (split_off_mut+Rayon, sharded-vec-writer,
atomic↔non-atomic, `reuse_vec`) are most useful in CPU-bound batch tools.
ucracc-lsp's hot path is < 10 µs and dominated by SQLite + tantivy — none
of those tricks would be measurable here. They're the right tools when
`crabcc index` ingests a 100k-file monorepo; that's a different crate.
