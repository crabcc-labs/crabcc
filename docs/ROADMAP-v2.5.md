# crabcc v2.5 — Roadmap

**Status (2026-04-30):** v2.0.0 + v2.1.0 tagged. v2.5 is the next minor —
its job is to make `crabcc memory` actually useful for *retrieval* and
finish the v2.0 distribution work that didn't make the cut.

## v2.0 reconciliation

| # | Title | State | Outcome |
|---|---|---|---|
| [#1](https://github.com/peterlodri-sec/crabcc/issues/1) | FSST string compression | closed | shipped (foundation in v2.0.0-alpha; drawer-body integration in v2.0.0) |
| [#3](https://github.com/peterlodri-sec/crabcc/issues/3) | edges-at-extract O(n²)→O(n) | closed | shipped in v2.0.0 |
| [#4](https://github.com/peterlodri-sec/crabcc/issues/4) | language coverage (Go / Python / Rust) | closed | shipped in v1.1.0 |
| [#6](https://github.com/peterlodri-sec/crabcc/issues/6) | CI optimizations | closed | shipped pre-2.0.0 |
| [#5](https://github.com/peterlodri-sec/crabcc/issues/5) | distribution + DX (install / brew / mdBook / demos) | open | **moved → v2.5** |
| [#2](https://github.com/peterlodri-sec/crabcc/issues/2) | memory epic (MemPalace port) | open | **kept open in v2.5** — M0 + M3-light shipped in v2.0.0; M0.5–M2 + M3-full pending |

PR [#15](https://github.com/peterlodri-sec/crabcc/pull/15) (build provenance + `crabcc info`) stays milestone-less — orthogonal plumbing, can land any time.

## v2.5 — goal

Make `crabcc memory search` actually semantic. Ship distribution polish.
Two sprints, hard cap.

**Theme:** *retrieval + distribution*. No net-new languages, no new sidecars.

**Window:** 4 weeks from v2.1.0 tag. Target: **2026-05-30**.

## Sprint 1 — semantic memory (2 weeks)

The v2.0.0 memory work landed M0 (Backend trait + persistent SqliteBackend +
PalaceRegistry) and M3-light (CLI + MCP surface). It captures and
keyword-searches but cannot do semantic retrieval. Sprint 1 closes that loop.

| ID | Deliverable | Tracks | Verify |
|---|---|---|---|
| 2.5.1 | `sqlite-vec` integration behind cargo feature `memory-vec` | M0.5 | feature builds clean on Linux + macOS CI |
| 2.5.2 | `fastembed-rs` (MiniLM-L6-v2) `Embedder` impl behind feature `memory-embed` | M1 | 384-dim vectors, ≤ 200 ms p95 / 100 docs on M1 |
| 2.5.3 | Drawer schema: `embedding BLOB` column + ANN index; idempotent `ALTER` | M0.5 | additive migration — v2.0 stores upgrade in place |
| 2.5.4 | `crabcc memory search QUERY [--k N] [--wing W]` returns vec-ranked hits when the feature is on; falls back to keyword otherwise | M1 | golden test: capture 50 lines, recall@5 ≥ 0.8 |
| 2.5.5 | MCP `memory_search` mirrors the CLI ranking change | M1 | smoke test in the MCP harness |

**Sprint 1 exit:** `cargo build -p crabcc-memory --features memory-vec,memory-embed` is green on both targets; smoke test shows semantic recall on a canned 50-line corpus.

## Sprint 2 — distribution + polish (2 weeks)

| ID | Deliverable | Tracks | Verify |
|---|---|---|---|
| 2.5.6 | BM25 hybrid: weighted keyword + vec blend in `memory search` | M2 | golden test on a "semantic-distractor" set |
| 2.5.7 | Brew tap published at `peterlodri-sec/homebrew-crabcc` (formula targeting v2.1.0+) | [#5](https://github.com/peterlodri-sec/crabcc/issues/5) | `brew install peterlodri-sec/crabcc/crabcc` works |
| 2.5.8 | `install.sh` upgrade path (compare local vs latest, pull binary) | [#5](https://github.com/peterlodri-sec/crabcc/issues/5) | manual test on macOS + Linux |
| 2.5.9 | mdBook docs site (basic, GH Pages) — README + ARCHITECTURE + ROADMAP rendered | [#5](https://github.com/peterlodri-sec/crabcc/issues/5) | site loads at `peterlodri-sec.github.io/crabcc` |
| 2.5.10 | `crabcc memory forget --drawer ID` + `--wing W --before DATE` + `VACUUM` | M3 | tests for both modes |
| 2.5.11 | `ARCHITECTURE.md`: add edges + FSST + memory subsystem diagrams | docs | mermaid renders; reviewed |
| 2.5.12 | Bench rerun post-edges + post-FSST; README + `REPORT.md` updated | docs | charts regenerated; numbers match main |

**Sprint 2 exit:** `v2.5.0` tagged; `brew install` path live; README still
accurate.

## Out of scope (explicit non-goals)

- New language extractors (Java / C# / Swift / Kotlin) → v2.6+
- VS Code / JetBrains extensions → v3.0
- Multi-repo / workspace federation → v3.0
- Graph visualization UI → v3.0
- Cloud / sync / multi-user memory → **never** (local-first is the product)
- Memory M4 KG ops → v2.6+

## Risk register

- **Embedding model size** — bundling MiniLM (~25 MB) inflates the binary.
  *Mitigation:* gate behind `memory-embed` feature, default off, document
  the separate download path.
- **sqlite-vec maturity** — vendor extension, evolves fast.
  *Mitigation:* pin version, cover with golden tests; the existing
  `Backend` trait abstracts it so a swap to `usearch` or `lancedb` is a
  1-file change.
- **Brew tap upkeep** — formula `sha256` rotates per release.
  *Mitigation:* CI job emits the formula skeleton on tag push; manual
  paste into the tap repo for now. CI auto-PR is a v2.6 follow-up.

## Acceptance — v2.5.0 ships when

1. All 12 deliverables merged.
2. `cargo nextest run --workspace --features memory-vec,memory-embed` green on Linux + macOS.
3. `brew install peterlodri-sec/crabcc/crabcc` installs v2.5.0.
4. README "Memory" section shows a real semantic-search example with real output.
5. `crabcc memory search` recall@5 ≥ 0.8 on a 1 000-message captured corpus.
6. Bench numbers in README reflect current main.
