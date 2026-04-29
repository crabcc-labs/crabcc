# Storage Backend Research for crabcc

**Workload (from the request):** 13k files / 38k symbols / ~9 MB DB today; upper bound ~400k symbols / ~100 MB. Hot path is `find_by_name(name)` (90%+ of queries). Single-process CLI: open DB, run query, close. Cold-start latency dominates per-invocation cost.

## Comparison table

| Option | License | Lang | Embed | Cold-start vs SQLite | Query model | Schema/typing | Single-binary | Write amplification | Wins over SQLite for crabcc? |
|---|---|---|---|---|---|---|---|---|---|
| **SQLite (rusqlite 0.31, bundled)** | Public domain | C | yes | baseline (~1–5ms open + plan) | SQL + indexes + FTS5 | DDL, ad-hoc indexes | yes (bundled) | low (B-tree, WAL) | — |
| **redb** ([cberner/redb](https://github.com/cberner/redb)) | MIT/Apache | Pure Rust | yes | likely faster open (mmap COW B-tree, no SQL planner); claims uncertain | typed `TableDefinition<K,V>` + secondary tables | strongly typed at compile time; no DDL | yes | moderate (COW B-tree) | typed-tables, no SQL parser, simpler binary |
| **fjall** ([fjall-rs/fjall](https://github.com/fjall-rs/fjall)) | MIT/Apache | Pure Rust | yes | LSM open does manifest read; comparable | KV + range + transactional partitions | bytes; you choose codec | yes | LSM (compaction) | great for write-heavy; not our profile |
| **sled** ([spacejam/sled](https://github.com/spacejam/sled)) | MIT/Apache | Pure Rust | yes | fast open | KV trees | bytes | yes | high (sometimes) | **avoid** — own README says "if reliability is your primary constraint, use SQLite. sled is beta… on-disk format will change" |
| **heed/LMDB** ([meilisearch/heed](https://github.com/meilisearch/heed)) | MIT | C (LMDB) + Rust | yes | excellent (mmap) — sub-ms open | typed via codec traits | typed env+db | yes (LMDB is small C) | low (COW B-tree) | fastest mmap reads; needs C toolchain on Windows |
| **rust-rocksdb** | Apache-2.0 | C++ | yes | slow (manifest scan, hundreds of ms not unusual) | KV + CFs | bytes | bloats binary (~10–20 MB) | LSM, heavy | **no** — overkill, fat binary |
| **DuckDB** ([duckdb/duckdb-rs](https://github.com/duckdb/duckdb-rs)) | MIT | C++ | yes | slow (analytical engine init) | full SQL, columnar | DDL | bundled adds 30–60+ MB | columnar | **no** for OLTP point lookups |
| **TinyDB / JSON file** | — | — | n/a | scales linearly with file size; load+parse 100MB JSON = seconds | doc scan | dynamic | yes | rewrite whole file | no |
| **In-memory map + serialized file (postcard/bincode/rkyv)** | — | Pure Rust | n/a | rkyv = mmap+zero-copy = sub-ms even at 100MB; postcard/bincode = 50–500ms parse for 100MB (uncertain, depends on schema) | iterate / hash | strongly typed | yes | full file rewrite (or shard per-file) | **yes for v1** if we accept full-rewrite on update |
| **Tantivy** ([quickwit-oss/tantivy](https://github.com/quickwit-oss/tantivy)) | MIT | Pure Rust | yes | "<10ms startup, perfect for CLI tools" (their words) | inverted index, BM25, fuzzy, prefix | schema builder | yes | segment merges | enables fuzzy/prefix that SQLite LIKE can't do well |

## Answers to your specific questions

**Is SQLite genuinely heavier in cold-start?** Probably not by enough to matter. A bundled rusqlite open + prepared-statement + indexed lookup is typically a few ms. The "heavy" feeling is about the *binary size and dependency surface* (SQLite amalgamation ~7 MB, plus your code), not query latency. I don't have authoritative head-to-head numbers for redb vs SQLite cold-start at this scale and won't bluff them.

**In-memory + serialized file at your sizes:**
- 9 MB index: any format loads in <50ms. Postcard ([djkoloski rust_serialization_benchmark](https://github.com/djkoloski/rust_serialization_benchmark)) and bincode are fine.
- 100 MB index: postcard/bincode realistically 200–800ms parse — *uncertain*, depends on Vec<String> count. **rkyv** ([rkyv/rkyv](https://github.com/rkyv/rkyv)) wins decisively here: zero-copy access via mmap, you skip parsing entirely. This is the only format that stays sub-10ms at 100 MB.

**Cleanest incremental update (per-file replace):** Any KV with prefix-scan (redb, heed, fjall, RocksDB) trivially does "delete all keys with prefix `file_id/...`, insert new ones." SQLite does this fine too with `DELETE WHERE file_id=?`. **In-memory + single-file rkyv is the worst here** — every file change rewrites the whole archive. A sharded layout (one rkyv file per source file, plus an index) fixes that, but you've reinvented a KV store badly.

**Adding `edges` later without migration pain:** redb (just declare a new `TableDefinition`), heed (new sub-DB), fjall (new partition) all win — no DDL, no migration. SQLite needs `CREATE TABLE` + a migration step. Not a big deal but real.

**Hybrid (KV + Tantivy):** Overcomplicated for v1. Two stores to keep consistent, two cold-starts. Worth it only when fuzzy/prefix becomes a headline feature.

## Recommendations

**v1 — stay on SQLite.** Reasons:
1. The "heavy" critique is aesthetic. At 9 MB and ~38k symbols an indexed `find_by_name` is sub-millisecond after open; the open itself is a few ms. You will not feel a difference swapping to redb, and you'll lose `EXPLAIN`, ad-hoc SQL, and the ability for users to inspect the DB with `sqlite3`.
2. SQLite's debuggability and ubiquity (any agent can `sqlite3 ./.crabcc.db "select …"`) is a real product feature for an "AI-coding-agent cache."
3. Single bundled binary already works on macOS/linux/windows.

**v1.5 / v2 — two changes worth considering, in this order:**
1. **Add Tantivy as a sidecar index** when you ship fuzzy/prefix name search. Don't replace SQLite; index just `symbols.name` into Tantivy. ~10ms startup matches our budget.
2. **If you ever want to drop the C dep entirely** (e.g. WASM build, or distributing via crates.io with no `cc` toolchain), swap SQLite → **redb**. It's the closest semantic match: ACID, typed tables, single file, pure Rust, stable format. Avoid sled (own README warns against it for reliability), avoid RocksDB/DuckDB (binary size), avoid hand-rolled rkyv (incremental updates get ugly).

**Don't pick:** sled (beta, format unstable), RocksDB (binary bloat), DuckDB (analytical engine for an OLTP workload), TinyDB (no point at 100k+ symbols).

## Uncertain claims (flagged)

- Specific cold-start ms for redb vs SQLite at 9MB / 100MB — I did not find authoritative head-to-head numbers in this research and won't fabricate them. Worth a 30-line benchmark in `bench/` before any swap decision.
- DuckDB binary size delta — "30–60 MB" is a rough industry figure, not measured for your build.
- Postcard/bincode parse times at 100MB — order-of-magnitude estimate; depends heavily on string count and allocator.
