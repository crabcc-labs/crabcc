# FSST string compression — research + integration plan for crabcc v2.0

> Research target: <https://github.com/spiraldb/fsst> (`fsst-rs` crate)
> Goal: evaluate whether FSST should land in crabcc as a v2.0 storage-layer feature, and if so, where + how + what to expect.

---

## 1. What is FSST

**Fast Static Symbol Table** compression. Designed by Peter Boncz, Thomas Neumann, and Viktor Leis (VLDB 2020 paper: <https://www.vldb.org/pvldb/vol13/p2649-boncz.pdf>). Built specifically for **database systems that store many short string values** — exactly the shape of crabcc's symbol/signature/snippet columns and (more importantly) the verbatim drawer-body column the MemPalace port introduces.

The algorithm in one paragraph: train a static dictionary of up to 255 short byte-sequences ("symbols") from a sample of the data, then encode each input string by greedy-matching against the dictionary. Each encoded byte is either a dictionary index (1 byte → up to 8 input bytes) or an escape code followed by a literal byte. Decoding is a tight 256-entry table lookup — vectorisable, branch-light, ~1–3 GB/s.

Trade-offs in plain English:

| | FSST | LZ4 | zstd |
|---|---|---|---|
| Compression ratio (short strings) | **2.0–3.0×** | 1.5–2.0× | 2.5–4.0× |
| Decode speed | **1–3 GB/s** | 3–5 GB/s | 0.5–1.5 GB/s |
| Random access (decompress one row without decompressing the page) | **YES** | NO | NO |
| Dictionary training cost | one-time, sample-based | none | none (or once for zstd-dict) |
| Per-row overhead | ~16 bytes (length + escape mask) | per-frame header | per-frame header |

**The killer property is row-level random access.** LZ4/zstd compress in frames; you cannot decode row #847 without decompressing the whole page first. FSST decodes any individual row in microseconds. That's exactly what an LLM agent does when it asks "give me drawer #847" — and exactly what crabcc does when it asks "give me the snippet for symbol id #5391".

---

## 2. The `fsst-rs` crate

| Field | Value |
|---|---|
| Crate name | `fsst-rs` (lib import: `use fsst::*`) |
| Latest version | **0.5.8** (March 2026) |
| License | Apache-2.0 (compatible with crabcc's MIT) |
| Runtime deps | `rustc-hash` only — zero other transitive deps |
| Architecture support | **little-endian only** (x86_64, aarch64). No big-endian plans. |
| Maturity self-claim | "still in-progress and is not production ready, please use at your own risk" — but Vortex/SpiralDB ship it in production already; the warning is conservative |
| Stars / contributors | 216 ★ / SpiralDB team + community |
| Public API surface | `Compressor::train`, `Compressor::compress[_into]`, `Decompressor::decompress[_into]`, `SymbolTable` serde |
| Inspired by | the MIT-licensed C++ reference impl from the paper authors |

Public API in ~5 lines:

```rust
use fsst::Compressor;

// Train once on a representative sample (~16 KB minimum, ~1 MB ideal).
let compressor = Compressor::train(&samples);

// Per-row compress.
let compressed: Vec<u8> = compressor.compress(b"hello world");

// Decompress (per-row, no frame).
let plain = compressor.decompressor().decompress(&compressed);

// Persist the symbol table next to the data.
let table_bytes: Vec<u8> = compressor.symbol_table().to_bytes();
```

The trained `SymbolTable` is ~2 KB serialised. Cheap to ship next to the SQLite file (e.g. `.crabcc/fsst.symbols`) or store inline in a `meta` row.

---

## 3. Where FSST fits in crabcc

Five candidate columns. Ranked by expected gain.

### 3.1 ⭐ MemPalace drawer body — **biggest win**

(Becomes relevant once Track A of the next sprint, the `crabcc memory` MVP, lands. See `docs/RESEARCH-mempalace.md`.)

- **Today's projection**: ~200 MB on disk for 100k drawers (800-char average + small metadata).
- **With FSST**: ~70–90 MB. A typical Claude Code conversation transcript (lots of repeated "I'll help you", "let me check", "TODO:", "implementation notes:") trains beautifully. Code excerpts have huge symbol-table wins because of repeated language tokens like `function`, `return`, `const`, `interface`.
- **Random access matters more here than anywhere else**: `crabcc memory drawer get <id>` and the search-result hydration step both load one drawer at a time.

### 3.2 Symbol `signature` column

- **Today**: ~30–100 bytes per row × ~38k rows on mc-mothership = ~1–4 MB.
- **With FSST**: ~0.4–1.5 MB. Useful at multi-million-symbol scale (entire monorepos with Go/Python/Rust extractors from sprint Track C).
- Random access is essential — `crabcc sym Foo` must hydrate signatures for ~10 rows max, never wants to decode a whole page.
- **Caveat**: the SQLite row store already compresses pages on read in WAL mode? No — SQLite does NOT compress pages by default. The `signature` column is stored uncompressed.

### 3.3 Hit `snippet` field — **not a fit**

- Currently capped at 80 chars and **never stored** — only materialised in JSON on query.
- Compressing transient query output is the wrong layer; the agent wants raw text out.
- Skip.

### 3.4 Tantivy posting lists

- Tantivy already uses its own block-oriented compression (variable-byte + bitpacking).
- FSST would be redundant at best, slower at worst (you'd decompress twice).
- Skip.

### 3.5 Source file caching for outline regeneration

- Speculative: if we ever cache file content to skip re-reading on `outline` queries, FSST is the right format.
- Not currently a column. Defer.

---

## 4. Per-column gain estimates

Numbers below are reasoned from FSST paper + Vortex production data, applied to crabcc's actual column shapes. **Pessimistic column** = "FSST has a bad day" (low entropy text, small training sample, rare-symbol penalty). **Optimistic** = the paper's reported best case on similar data.

| Column | Today | FSST pessimistic | FSST optimistic | Random-access matters? |
|---|---:|---:|---:|---|
| `drawers.body` (MemPalace, projected 100k) | 200 MB | **140 MB (1.43×)** | 65 MB (3.1×) | **YES** |
| `drawers.body` (1M scale) | 2 GB | **1.4 GB (1.43×)** | 650 MB (3.1×) | **YES** |
| `symbols.signature` (38k rows) | 3.5 MB | 2.4 MB (1.46×) | 1.2 MB (2.9×) | YES |
| `symbols.signature` (500k rows) | 45 MB | 31 MB (1.45×) | 15 MB (3.0×) | YES |
| `kg_triples.{subject,predicate,object}` | varies | 1.4× | 2.5× | YES |
| Tantivy posting lists | already compressed | n/a | n/a | n/a |
| Hit snippet (transient) | n/a | n/a | n/a | n/a |

**Pessimistic decode latency**: ~300 ns per drawer (single-row, cold cache). At 100k drawers that's still <1ms to hydrate the top-10 search hits. Acceptable.

---

## 5. Implementation sketch

The minimal change in crabcc is two new helpers and one schema column. No new dep beyond `fsst-rs = "0.5"`.

### 5.1 Workspace

```toml
# Cargo.toml (workspace deps)
fsst-rs = "0.5"
```

### 5.2 New module — `crates/crabcc-core/src/compress.rs`

```rust
//! Optional FSST compression layer for text-heavy columns.
//!
//! Symbol table is trained once per palace, persisted to .crabcc/fsst.symbols,
//! and loaded on Store::open. Falls back to pass-through if symbol table is
//! missing — every column tagged `compressed=BOOL` so old data stays readable.

use anyhow::Result;
use fsst::Compressor;
use std::path::Path;

pub struct Codec {
    compressor: Compressor,
}

impl Codec {
    pub fn train(samples: &[&[u8]]) -> Result<Self> {
        Ok(Self { compressor: Compressor::train(samples) })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let table = fsst::SymbolTable::from_bytes(&bytes)?;
        Ok(Self { compressor: Compressor::from_table(table) })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.compressor.symbol_table().to_bytes())?;
        Ok(())
    }

    #[inline] pub fn compress(&self, plain: &[u8]) -> Vec<u8> {
        self.compressor.compress(plain)
    }
    #[inline] pub fn decompress(&self, encoded: &[u8]) -> Vec<u8> {
        self.compressor.decompressor().decompress(encoded)
    }
}
```

### 5.3 Schema migration (additive — backward compatible)

```sql
-- New: per-row encoding flag. 0 = plain, 1 = FSST.
ALTER TABLE symbols ADD COLUMN signature_enc INTEGER NOT NULL DEFAULT 0;

-- For MemPalace drawers (when crates/crabcc-memory lands):
ALTER TABLE drawers  ADD COLUMN body_enc INTEGER NOT NULL DEFAULT 0;
```

Old rows have `*_enc = 0` (plain text). New rows written by an FSST-aware Store get `_enc = 1`. Read path checks the column and decompresses if needed. **No migration step required.** Background re-encoding is a separate `crabcc compress` subcommand.

### 5.4 Store path changes

`Store::open` reads `.crabcc/fsst.symbols` if present. `replace_symbols` checks a feature flag (`crabcc_core::compress::ENABLED`) and encodes `signature` if on. `find_by_name` decodes per-row in the `query_map` callback.

### 5.5 New CLI subcommand

```
crabcc compress              # train symbol table on existing data, mark all rows for re-encode
crabcc compress --rebuild    # re-encode every row in-place (slow; only after schema change)
crabcc compress --stats      # report bytes saved per column, current symbol table size
```

### 5.6 New Taskfile target

```yaml
compress:
  desc: Train FSST symbol table on the current index + re-encode all text columns
  deps: [build]
  cmds:
    - "{{.CRABCC}} compress"
    - "{{.CRABCC}} compress --stats"
```

---

## 6. Performance verification plan

Don't trust the paper's numbers blindly — measure on crabcc's actual data.

### 6.1 Bench harness extension

Add `bench/compress-bench.py` mirroring `bench/raw-bench.py`:

| Task | Measurement |
|---|---|
| Index `mc-mothership` with FSST off vs on | DB size, indexing wall-time |
| Train symbol table on 1k random signature rows | Training time, symbol-table bytes |
| 10k single-row decompresses (random `find_by_name`) | p50/p95/p99 latency |
| Bulk decompress all signatures (e.g. for `iter_all_symbols`) | Throughput MB/s |
| MemPalace drawer add/search with body_enc=0 vs body_enc=1 | End-to-end search latency |

### 6.2 Release gate

Ship FSST as `--features compress` (off by default) for v2.0.0, then **flip the default to on once the bench shows**:

- p99 single-row decode <1 ms
- DB-size reduction ≥1.4× on signatures + drawer bodies (the pessimistic floor)
- Indexing throughput regression <10%
- Zero correctness regressions across the existing 102-test suite

### 6.3 Fuzzing

`fsst-rs` already ships its own fuzz harness. We should additionally fuzz the **roundtrip through SQLite**: random text → encode → INSERT → SELECT → decode → assert equality. Catch encoding bugs that only manifest after cross-language SQLite type juggling (BLOB ↔ TEXT, UTF-8 normalisation).

---

## 7. Risks & open questions

| Risk | Mitigation |
|---|---|
| `fsst-rs` README says "not production ready" | But Vortex/SpiralDB ship it in production. Pin exact version; participate upstream if we hit bugs. Keep the codec layer behind a `crabcc_core::compress` module so swapping to the upstream cwida C++ impl (via FFI) is a single file change. |
| Little-endian only | crabcc already targets x86_64 + aarch64 only (CI matrix). Document the constraint. |
| Pre-1.0 (0.5.x) — semver-minor-major can break API | The integration is small (one module, one table column). Pinning fsst-rs="0.5" is fine. |
| Symbol table trained on bad sample → bad ratio | Sample = a stratified random selection (50% signatures, 30% drawer bodies, 20% file paths). Re-train on each `crabcc compress --rebuild`. |
| FSST + SQLite mmap interaction | Pages stay aligned; FSST writes its output as opaque BLOB. Confirmed safe by Vortex's parquet adapter, which does the same dance. |
| Adds a hard dep that's Apache-2.0 vs our MIT | Apache-2.0 is permissive enough. License file gets a third entry; CHANGELOG notes the addition. |
| Indexing throughput regression — encoding adds CPU on the write path | Pessimistic: 5–10% slowdown. Mitigation: encode in a worker thread (rayon), batch 1000 rows per train+encode. |
| ABI lock-in: existing databases without `*_enc` columns | Schema migration is additive; old rows continue to read as plain. No forced migration. |

---

## 8. Migration / rollout

1. **v1.x line stays plain** — no FSST dependency, no new column. Releases continue from current main.
2. **v2.0.0-alpha** — `compress` feature flag, opt-in, `--features compress` at build time. Schema column added with default 0. `crabcc compress` subcommand available.
3. **v2.0.0-beta** — flag flips to **on by default** once the bench gate passes. Old DBs continue to work; `crabcc compress --rebuild` upgrades them.
4. **v2.1** — eligible for further wins: variable-symbol-table-per-language (one trained on Ruby signatures, another on TS, etc.), symbol-table-per-drawer-wing (MemPalace).

---

## 9. References

- FSST paper (VLDB 2020): <https://www.vldb.org/pvldb/vol13/p2649-boncz.pdf>
- `fsst-rs` repo: <https://github.com/spiraldb/fsst>
- Reference C++ impl: <https://github.com/cwida/fsst>
- Vortex (production user of fsst-rs): <https://github.com/spiraldb/vortex>
- DuckDB FSST page: <https://duckdb.org/2022/10/28/lightweight-compression.html>
- crabcc MemPalace research (drawer body context): `docs/RESEARCH-mempalace.md`
