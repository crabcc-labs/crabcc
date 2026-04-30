# Internal Agent — crabcc-memory specialist

You own `crates/crabcc-memory/`. Read `internal_agents/shared.agent.md`
first. This file is the crate-specific context.

## What this crate does

`crabcc-memory` is the **AI memory layer** at `.crabcc/memory.db`
(per-repo). Issue #2's epic is closed end-to-end as of v2.5.x.

- **Storage** — `SqliteBackend` (WAL + FSST drawer-body compression),
  `sqlite-vec` ANN scaffold behind `--features memory-vec`.
- **Hybrid search** — FTS5 BM25 ⊕ cosine KNN fused via Reciprocal
  Rank Fusion (k=60). See `palace.rs` and `backend/sqlite.rs`.
- **Real embeddings** — `FastEmbedder` (MiniLM-L6-v2, 384-dim) behind
  `--features memory-embed`. ~25 MB ONNX, lazy-downloaded into
  `~/.cache/crabcc-memory/` on first use.
- **Miners** — `mine project` (repo files), `mine sessions` (Claude
  Code JSONL transcripts). Both idempotent.
- **Schema** — `crates/crabcc-memory/schema/001_init.sql`. Additive
  only; mirror `Store::open`-style migrations.

## Bench gate (must stay green)

`task memory-bench` runs the LongMemEval R@k harness in `bench/memory/`.
The fixture clears R@5 ≥ 96.6% under `lexical` and `hybrid` modes.
For real LongMemEval: `DATASET=path/to/longmemeval_oracle.json
task memory-bench`.

## Conventions specific to this crate

- **The Backend trait is the seam.** SqliteBackend is the only impl
  today; an in-memory backend for tests + a future remote one are
  on the roadmap. Don't leak SQLite types past the Backend boundary.
- **Drawer bodies are FSST-compressed at write time.** Read paths
  decode lazily. Don't bypass the codec without bench gates.
- **Embedder selection is feature-gated.** `HashEmbedder` (default,
  0-byte) vs `FastEmbedder` (memory-embed feature). Keep both paths
  passing `cargo test --no-default-features` and
  `cargo test --features memory-embed`.

## Cross-crate boundaries

- Consumed only by `crabcc-cli` (the `memory` subcommand and the
  miners). The MCP server exposes `memory.*` tools through the CLI's
  shim, not directly.
- Does NOT depend on `crabcc-core`. The memory layer is independent
  of the symbol index — different DBs, different concerns.
