# Performance Review Replay: crabcc Audit → Fix Session

**Date:** 2026-06-11  
**Project:** `/Users/lodripeter/workspace/peterlodri-sec/crabcc`  
**Purpose:** Capture the performance review/fix session in a replay-friendly format for a later research blogpost episode.

---

## Episode Framing

This session starts with a broad performance audit of `crabcc`, then narrows into a fix round around the highest-impact bottlenecks.

The interesting tension: `crabcc` already has several good low-level performance patterns — thread-local tree-sitter parsers, bump arenas, SQLite WAL, reusable JSON buffers — so the bottlenecks are not obvious micro-optimizations. The audit pushes us toward pipeline shape: where data is collected, where work is serialized, and where SQLite is intentionally single-writer.

---

## Cast

| Role | Focus |
|---|---|
| Prime reviewer | Map stack, layers, concurrency model, hot paths |
| Architecture auditor | Identify system-level performance risks |
| Implementer | Turn audit findings into surgical code changes |
| Verifier | Run targeted tests/benchmarks and record evidence |

---

## Source Artifacts

- `review-prime.md` — architecture and stack prime
- `review-audit.md` — architectural performance audit
- `crates/crabcc-core/src/index.rs` — indexing and refresh pipeline
- `crates/crabcc-core/src/store.rs` — SQLite-backed `Store`
- `docs/bench/` — benchmark and performance history

---

## Baseline Observations

From `review-audit.md`:

1. **Existing strengths**
   - Rayon parallel extraction for full index builds.
   - Thread-local parser pool avoids cross-thread parser locks.
   - Per-file `Bump` arenas reduce allocation/drop churn.
   - SQLite configured with WAL, `NORMAL` sync, mmap, 64MB cache.
   - MCP stdio path reuses buffers and avoids needless UTF-8/string conversions.

2. **Top risks**
   - **Risk 1:** Full index build collects all extracted parse output before SQLite writes.
   - **Risk 2:** `Store` is guarded by `Mutex`, serializing read-only MCP queries.
   - **Risk 3:** `refresh_delta` handles changed-file read/hash/parse sequentially.

3. **Current focus**
   - Start with `refresh_delta`, because it is local to `index.rs`, has a clear parallelizable stage, and affects branch-switch/watch refresh workflows.

---

## Current `refresh_delta` Shape

File: `crates/crabcc-core/src/index.rs`

Current flow:

1. Load DB file metadata via `store.list_files_with_meta()`.
2. Walk repository with `walker::walk_repo(root)`.
3. For each path sequentially:
   - Detect language.
   - Compute relative path.
   - Read mtime.
   - If mtime unchanged: skip.
   - If mtime changed: read full file, hash bytes, maybe parse and persist.
   - If new file: read, hash, parse and persist.
4. Delete rows missing from disk.
5. Sort delta buckets for deterministic output.

Key hot section:

```text
index.rs:277-353
walk -> metadata -> read -> hash -> extract -> persist
```

The fast path is good: unchanged mtimes avoid reads entirely.  
The slow path is serial: a checkout touching hundreds of files pays read/hash/parse one file at a time.

---

## Fix Hypothesis

`refresh_delta` should mirror the good part of `build_index_with_progress`:

- Keep SQLite writes sequential.
- Move CPU-heavy and I/O-heavy changed-file extraction into a parallel phase.
- Preserve existing semantics:
  - same stats
  - same added/modified/removed buckets
  - same deterministic sorting
  - same `MAX_FILE_BYTES` cap
  - same parse error accounting

Target design:

```text
Phase 1: classify changed/new/removed files
Phase 2: parallel read + hash + extract for changed/new candidates
Phase 3: sequential SQLite persistence
```

This should improve refreshes after branch switches or large generated-code changes while keeping the common unchanged fast path cheap.

---

## Review Round 1 Plan

### Goal

Refactor refresh into a parallel extraction pipeline without changing public API or visible output.

### Candidate Implementation Steps

1. Introduce internal candidate/outcome structs:
   - `RefreshCandidate`
   - `RefreshOutcome`
   - maybe reuse existing `ExtractedFile` / `FileOutcome` where appropriate.

2. First pass over walker:
   - count unsupported/unreadable metadata cases
   - collect candidates needing byte read/hash/parse
   - collect untouched/unchanged stats
   - collect `seen` for deletion handling

3. Parallel process candidates with Rayon:
   - read bytes
   - enforce `MAX_FILE_BYTES`
   - hash bytes
   - if modified candidate hash equals stored sha, emit `Touched`
   - otherwise extract symbols/edges into `ExtractedFile`

4. Sequentially apply outcomes:
   - `Touched` → `store.touch_mtime`
   - `Extracted` → `store.upsert_file` + replace symbols/edges via existing persistence path
   - errors → stats only

5. Preserve stable result ordering:
   - sort `added`, `modified`, `removed` at end.

### Risks

| Risk | Guard |
|---|---|
| Changed stats drift | Existing tests around refresh deltas; add/adjust targeted test if missing |
| SQLite write order assumptions | Keep persistence sequential |
| Extra memory from collecting candidates | Candidates are path/meta only, much smaller than full extracted output |
| Small refresh overhead | Use sequential path or threshold if benchmark shows regression |

---

## Review Round 2 Plan

Round 2 should only happen after Round 1 compiles and targeted tests pass.

Possible focus depending on evidence:

1. **If refresh improved and tests pass:**
   - Add/adjust benchmark notes.
   - Document parallel refresh behavior in code comments.

2. **If small refresh regresses:**
   - Add threshold: sequential for small candidate count, parallel for large candidate count.

3. **If memory grows too much:**
   - Replace full outcome collection with bounded channel from parallel workers to sequential writer.
   - This is more complex and should only happen with evidence.

4. **If lock contention becomes dominant:**
   - Defer to separate `Mutex<Store>` → `RwLock` review; larger surface area across MCP/watch.

---

## Evidence To Capture

Before claiming a win, record:

```bash
# Build/check
cargo test -p crabcc-core refresh
cargo test -p crabcc-core index

# If benchmarks exist
cargo bench -p crabcc-core --bench <bench-name>

# Manual scenario
# 1. Build index on a medium repo
# 2. Touch/modify N files
# 3. Run refresh before/after
```

Record:

| Check | Before | After | Notes |
|---|---:|---:|---|
| `cargo test -p crabcc-core refresh` | TBD | TBD | Must pass |
| large refresh wall time | TBD | TBD | target scenario |
| memory peak | TBD | TBD | optional |

---

## Blogpost Angle

Working title ideas:

- **When Fast Code Is Still Slow: Finding Pipeline Bottlenecks in a Rust Indexer**
- **The Performance Bug Wasn't a Clone: It Was a Queue Shape**
- **Parallel Parsing, Single-Writer SQLite: Tuning crabcc's Refresh Path**

Narrative beats:

1. The easy answer would be micro-optimizing allocations.
2. The audit shows the code already has strong local performance discipline.
3. The real issue is phase coupling: refresh serializes read/hash/parse/write.
4. The fix is not “make SQLite concurrent”; it is to parallelize before the write boundary.
5. The lesson: in Rust systems, performance wins often come from pipeline topology, not heroic unsafe code.

---

## Session Log

### 2026-06-11 — Audit Imported

- Loaded `review-audit.md`.
- Confirmed top risks:
  - full build memory fan-in
  - MCP read lock contention
  - sequential refresh changed-file path
- Inspected `refresh_delta` in `crates/crabcc-core/src/index.rs:269-379`.
- Chose refresh parallelization as the first surgical target.

### 2026-06-11 — Next Action

Implement Round 1 parallel extraction for refresh, then run targeted tests and update this log with observed results.
