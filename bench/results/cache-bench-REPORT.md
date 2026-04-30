# moka cache bench — issue #30

_Generated: 2026-04-30 01:46 UTC on HFK99M239G (arm64)._

Three cache sites, all backed by `moka::sync::Cache`. Numbers are in-process micro-benches built with the workspace release profile (LTO=fat, codegen-units=1).

## Summary

| Site | Pre-cache | Post-cache | Speedup |
|---|---|---|---|
| PalaceRegistry::open_for (cold→warm) | 1.95 ms | 6.18 µs | **315.3×** |
| find_git_root (raw→memo) | 14.17 µs | 197 ns | **71.9×** |
| Embedder::embed_one (HashEmbedder→Cached) | 1.01 µs | 257 ns | **3.9×** |

## 1. PalaceRegistry::open_for

- Cold open (one shot): **1.95 ms** — SQLite file create + schema migration + dir walk for `.git`.
- Warm open (avg over 1,000 calls): **6.18 µs** — pure moka hit.
- Speedup: **315.3×**.

Real-world impact: the MCP server gets one `cwd` arg per tool call. Without the cache every call re-opens SQLite. With it, each repo's palace is reused across the 10-min idle window.

## 2. find_git_root memo (60 s TTL)

- Raw walk (avg over 10,000 iters): **14.17 µs** — `canonicalize` + ancestor scan for `.git/`.
- Memoized (avg over 10,000 iters): **197 ns** — moka hit, no syscalls.
- Speedup: **71.9×**.

Tiny win individually, multiplies across every MCP tool call.

## 3. CachedEmbedder

- HashEmbedder direct (avg over 5,000 iters): **1.01 µs** — FNV + xorshift fill.
- CachedEmbedder over 16 distinct bodies (5,000 calls, ~312× hit ratio): **257 ns** — sha256 lookup + Arc clone.
- Speedup: **3.9×** today; expected 100–1000× once `FastEmbedder` (issue #18) lands and the inner cost is ONNX inference instead of a hash fill.
- Cache entries after the run: 16 (max capacity defaults to 4 096).

## Out-of-scope (per issue #30)

- `sym` / `refs` / `callers` query results — SQLite is already sub-ms; moka would add memory pressure for marginal wins.
- FSST decoders — already `Arc<Codec>` per Store, no contention.

