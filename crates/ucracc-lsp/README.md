# `ucracc-lsp`

> Deeper docs: see [`docs/HOW_IT_WORKS.md`](docs/HOW_IT_WORKS.md) for the
> user-facing LSP method reference, custom command catalog, and the
> developer-facing architecture / extension guide.

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
| `workspace/symbol`               | native `Fts::prefix`                         |
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
| Swift           | ✓      | ✓     | crabcc-core extractor *(moved in v0.2)*|
| Bash            | ✓      | ✓     | crabcc-core extractor *(moved in v0.2; commands → call edges)* |
| YAML            | ✓      | —     | this crate (tree-sitter-yaml) — top-level keys, no edges |
| Markdown        | ✓      | —     | this crate (tree-sitter-md) — heading outline, no edges |

**Why some languages moved**: Swift and Bash *are code* — putting them in
`crabcc-core` means `crabcc index`, `crabcc sym`, `crabcc callers`, and
the MCP server all pick them up automatically, not just this LSP. YAML
and Markdown aren't code; they stay here to keep `crabcc-core`'s mission
focused on symbol extraction.

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
# build from source
cargo build --release -p ucracc-lsp
# the binary lives at target/release/ucracc-lsp; it speaks LSP over stdio

# or install onto $PATH
cargo install --path crates/ucracc-lsp
```

### Prebuilt releases

Each `ucracc-lsp-v*` tag publishes a [GitHub release](https://github.com/crabcc-labs/crabcc/releases)
with per-target tarballs (linux x86_64 / aarch64, macOS aarch64), each
accompanied by a `.sha256` checksum — and, once signing is enabled, a cosign
`.cosign.bundle`. A multi-arch **Docker image** is also published:

```bash
docker pull crabcc-labs/ucracc-lsp:0.4.0     # or :latest
```

Verifying downloads (checksums always; cosign once keys/secrets are wired) is
documented in [`docs/COSIGN-SETUP.md`](../../docs/COSIGN-SETUP.md).

**Zed** — install the [`editors/zed/crabcc`](../../editors/zed/crabcc) extension
(`zed: install dev extension` → pick that dir). Zed can't bind a new LSP
binary to a language from `settings.json` alone, so the extension is the
supported path. Full guide: [`docs/ZED.md`](docs/ZED.md). TL;DR:

```bash
cargo install --path crates/ucracc-lsp   # binary on $PATH
crabcc index                             # build the index in your project
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

ucracc-lsp expects `.crabcc/index.db` (fuzzy/prefix for `workspace/symbol`
are built in-memory from it — no separate sidecar) for
workspace symbol) to exist. Run `crabcc index` in the repo once.

### `initialization_options`

Clients can override where the server looks for the index via the
standard LSP `initialization_options` blob:

| Key | Type | Meaning |
|---|---|---|
| `indexPath` (a.k.a. `index_path`) | string | Path to the `.crabcc` dir holding `index.db`. Relative paths resolve against the workspace root; absolute paths are used as-is. Default: `<root>/.crabcc`. |

Handy for out-of-tree indexes (CI artifacts, shared caches) and remote
hosts where the `.crabcc` dir sits outside the checkout. It overrides the
index *location* only — the index must still have been built **for the
same workspace root** (stored file paths are relative to it), so pointing
at a different root's index (e.g. a subcrate reading the parent monorepo's
`.crabcc`) is not supported. In Zed, set it under
`lsp.ucracc-lsp.initialization_options`.

## Performance

`cargo bench -p ucracc-lsp --bench baseline_vs_lsp` — measured on M-series
Apple silicon, dev fixtures (single Rust file, ~10 symbols, 1 call edge).
**Baseline** = calling `crabcc-core` directly. **LSP cold** = first call
after cache flush. **LSP cached** = repeat of the same call in the same
edit-session.

| Operation                | Baseline | LSP cold | LSP cached | Cache win |
|--------------------------|----------|----------|------------|-----------|
| `initialize` only (lazy) | n/a      | **24 µs**| n/a        | n/a       |
| `initialize + initialized` | n/a    | 1.01 ms  | n/a        | n/a       |
| `documentSymbol`         | 8.2 µs   | 8.7 µs   | 3.7 µs     | 2.4×      |
| `definition`             | 6.2 µs   | 7.2 µs   | 1.1 µs     | 6.5×      |
| `hover`                  | 6.2 µs   | 6.9 µs   | 1.2 µs     | 5.7×      |
| `workspace/symbol`       | 601 µs   | 604 µs   | 1.1 µs     | **550×**  |

`initialize` returns in tens of microseconds because Store + Fts are
**lazy-opened**. The 1 ms SQLite open + symbol-load happens on the background
`initialized` notification; if the user never sends a request, no I/O
ever happens. First request after a cold launch pays the open cost once
(then never again until process restart).

### Tree-sitter pipeline (v0.2)

| Op | Cost | Notes |
|---|---|---|
| Full reparse — 100-fn Rust file (~3 KLOC) | 1.32 ms | Cold parse |
| Incremental reparse — same file, 1-byte edit | **162 µs** | **8.2× faster** |

Per-doc `Tree` cache + LSP `INCREMENTAL` sync. Each `didChange` produces
`InputEdit`s that are applied to the cached tree; `Parser::parse(src,
Some(&old_tree))` skips subtrees outside the edit region. Combined with
the thread-local Parser pool in `crabcc-core` (one Parser per thread per
language, reused across calls), keystrokes in big files stay sub-200 µs.

The dispatch wrapper adds < 1 µs per hot-path call (URL parse + Mutex
acquire + Tokio task hop). Cold start is dominated by the SQLite open
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
  allocator overhead in the search + serde_json hot paths. Cheap to add
  behind a Cargo feature.
- **Linking with [Wild](https://github.com/davidlattimore/wild)** on
  Linux. Shaves dev-loop link time once it's stable enough for our
  build matrix.
- **Cross-process FTS preview cache.** `workspace/symbol`'s 604 µs is
  almost entirely the symbol scan. A small LRU on (query, top-K) would cut p99
  on repeated typing.
- **Reuse the parsed-tree across `didChange` events.** tree-sitter
  supports incremental reparse — we currently throw the tree away. Would
  help large files (>10 KLOC) but isn't measurable on the bench fixtures.

Lattimore's "Wild performance tricks" (split_off_mut+Rayon, sharded-vec-writer,
atomic↔non-atomic, `reuse_vec`) are most useful in CPU-bound batch tools.
ucracc-lsp's hot path is < 10 µs and dominated by SQLite + the symbol scan — none
of those tricks would be measurable here. They're the right tools when
`crabcc index` ingests a 100k-file monorepo; that's a different crate.
