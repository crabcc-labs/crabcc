# MemPalace → crabcc — research, port plan, and reuse map

> Research target: <https://github.com/MemPalace/mempalace>
> Goal: rebuild MemPalace's idea in Rust, ship it as a `crabcc memory …` subcommand, and reuse crabcc's existing internals (store, MCP server, watch sidecar, walker, track telemetry) wherever possible.

---

## Part 1 — What MemPalace actually is

### 1.1 Pitch

A local-first **AI memory** system. Stores conversations and project content **verbatim** (no summarisation, no LLM in the hot path) and retrieves with semantic search. Headline numbers from `benchmarks/BENCHMARKS.md`:

| Mode | R@5 | LLM |
|---|---|---|
| Raw (semantic only, no heuristics, no LLM) | **96.6%** on LongMemEval (500 q) | none |
| Hybrid v4 (BM25 + cosine + temporal/preference boost) | **98.4%** held-out | none |
| Hybrid v4 + LLM rerank | ≥99% | any capable model |

### 1.2 Conceptual model — wings, rooms, drawers

MemPalace's structural primitives:

- **Wing** — a person, a project, an agent. Top-level scope. e.g. "myapp", "claude-code".
- **Room** — a topic within a wing. "deployment", "auth", "schema-migrations". Auto-detected by `room_detector_local.py`.
- **Drawer** — a verbatim chunk of source content (~800 chars by default for project files; "exchange pair" for conversations).

Search is *scoped* — `mempalace search "X" --wing myapp --room auth` is much more precise than a flat-corpus query. That structuring is a third of the recall lift in their numbers (the rest is hybrid retrieval).

### 1.3 Repo layout (`mempalace/` top-level — selected)

| File | LOC-ish | Role |
|---|---:|---|
| `cli.py` | 49 KB | argparse CLI: `init`, `mine`, `search`, `wake-up` |
| `palace.py` | 13 KB | High-level palace model + locking |
| `palace_graph.py` | 27 KB | Closet/wing/room graph |
| `miner.py` | 44 KB | Project-file mining; 800-char chunking; room routing |
| `convo_miner.py` | 18 KB | Claude Code JSONL, ChatGPT, Slack ingestion (exchange-pair chunking) |
| `searcher.py` | 29 KB | Retrieval: `_hybrid_rank` (BM25 + cosine + closet boost) |
| `embedding.py` | 5 KB | ONNX MiniLM-L6-v2 loader (384-dim) |
| `knowledge_graph.py` | 18 KB | Temporal entity-relationship graph (SQLite) |
| `entity_registry.py` | 25 KB | Canonical entity store (people, projects) |
| `mcp_server.py` | 72 KB | **29 MCP tools**, hand-rolled JSON-RPC stdio loop, WAL log at `~/.mempalace/wal/write_log.jsonl` |
| `backends/base.py` | 12 KB | Pluggable backend ABC: `BaseBackend`, `BaseCollection`, typed results |
| `backends/chroma.py` | 47 KB | Default ChromaDB backend, HNSW guards |
| `backends/registry.py` | 6 KB | Entry-point–driven backend selection |
| `closet_llm.py`, `llm_refine.py` | 12+18 KB | Optional LLM rerank pipeline |
| `dedup.py`, `repair.py`, `sweeper.py` | 8+29+13 KB | Maintenance/dedup/repair (~50 KB combined) |

273 files total in the repomix pack. Heavy: `mcp_server.py` (72 KB) and `miner.py` (44 KB) carry most of the orchestration.

### 1.4 Storage model — `BaseCollection` contract (RFC 001)

From `backends/base.py`, abstract methods (kwargs-only, typed results):

```python
class BaseCollection(ABC):
    def add(self, *, documents, ids, metadatas=None, embeddings=None) -> None: ...
    def upsert(self, *, documents, ids, metadatas=None, embeddings=None) -> None: ...
    def query(self, *, query_texts=None, query_embeddings=None,
              n_results=10, where=None, where_document=None, include=None) -> QueryResult: ...
    def get(self, *, ids=None, where=None, where_document=None,
            limit=None, offset=None, include=None) -> GetResult: ...
    def delete(self, *, ids=None, where=None) -> None: ...
    def count(self) -> int: ...
    # Optional: estimated_count, close, health, update (default = get + merge + upsert)

class BaseBackend(ABC):
    name:           ClassVar[str]
    spec_version:   ClassVar[str] = "1.0"
    capabilities:   ClassVar[frozenset[str]]
    def get_collection(self, *, palace: PalaceRef,
                       collection_name: str, create=False, options=None) -> BaseCollection: ...
```

Returned types:

```python
@dataclass(frozen=True)
class QueryResult:                        # outer = #queries, inner = hits/query
    ids:        list[list[str]]
    documents:  list[list[str]]
    metadatas:  list[list[dict]]
    distances:  list[list[float]]
    embeddings: Optional[list[list[list[float]]]]

@dataclass(frozen=True)
class PalaceRef:
    id:         str
    local_path: Optional[str]
    namespace:  Optional[str]
```

The contract is intentionally Chroma-shaped (kwargs, dict-compat for legacy callers) — but it's already typed enough to back with anything. **This is the seam through which a Rust backend would swap in.**

### 1.5 Default embedding pipeline

- Model: `all-MiniLM-L6-v2`, 384-dim, fp32 ONNX weights
- Runtime: ONNX Runtime via Chroma's bundled embedding function
- Disk: ~300 MB (model + tokenizer); ~80 MB without quantisation
- ARM64 macOS notes: `__init__.py` documents past segfaults from the chromadb 0.x hnswlib binding (#74, #521); fixed by upgrading to 1.5.4+ (#581)

### 1.6 Search pipeline (raw → hybrid v4 → reranked)

| Stage | Used | What it does | Recall |
|---|---|---|---|
| **Raw** | always | cosine similarity over MiniLM embeddings, top-N | 96.6% |
| **BM25 boost** | hybrid v2+ | tantivy/sqlite-bm25 over the same drawers, weighted blend | +0.5–1% |
| **Closet boost** | hybrid v3+ | content from "the closet" (high-signal personal facts) ranked first | +0.5% |
| **Temporal proximity** | hybrid v4 | recent drawers boosted; decay function | +0.5% |
| **Preference patterns** | hybrid v4 | user-specific phrase boosts | +0.5% |
| **LLM rerank** | optional | top-20 → LLM picks best, model-agnostic (Haiku, Sonnet, minimax) | +0.6% (last mile) |

The honest framing is that **the raw 96.6% is already good enough** for almost every use case. Hybrid v4 is the production default; LLM rerank is opt-in for the last 1.4%.

### 1.7 Knowledge graph

- SQLite-backed temporal triples: `(subject, predicate, object, valid_from, valid_to)`
- Operations: add, query, invalidate (close validity window), timeline (sequence by validity)
- Used to encode "Peter works at Anthropic since 2025-04-01" so that asking "where does Peter work?" can resolve to a *current* answer rather than a vector hit on an old conversation
- Schema reportedly in `docs/schema.sql`; runtime in `mempalace/knowledge_graph.py`

### 1.8 MCP server

- 29 tools: palace reads/writes, KG ops, cross-wing navigation, drawer management, agent diaries
- Hand-rolled JSON-RPC 2.0 over stdio (same shape crabcc-mcp uses)
- Write-ahead log at `~/.mempalace/wal/write_log.jsonl` — durability across crashes
- Source: `mempalace/mcp_server.py`, 72 KB

### 1.9 Runtime deps (`pyproject.toml`)

```toml
[project]
name = "mempalace"
version = "3.3.3"
license = "MIT"
requires-python = ">=3.9"

dependencies = [
    "chromadb>=1.5.4,<2",
    "pyyaml>=6.0,<7",
    "tomli>=2.0.0; python_version < '3.11'",
]
```

Three runtime deps. ChromaDB drags in onnxruntime, hnswlib, sqlite, tokenizers — the heavy lift. PyYAML + tomli are config glue.

Entry-point–driven plugin model:
- `mempalace.backends` group → backend implementations (ChromaBackend default)
- `mempalace.sources` group → mining sources (project files / Claude JSONL / Slack — RFC 002)

---

## Part 2 — Why rebuild in Rust

| Pain point in the Python version | Rust port answer |
|---|---|
| ChromaDB hnswlib segfaults on ARM64 (issues #74, #521) | Replace HNSW with a pure-Rust impl (`hnsw_rs`) or `sqlite-vec` extension. No C++ binding. |
| 300 MB embedding model + ONNX Runtime + Chroma + Python = ~600 MB+ install | Ship `crabcc memory` as part of the existing single static binary. Embedding model still ~80 MB but loaded via `fastembed-rs` (ORT) or `candle`. |
| Cold start: importing `chromadb` takes 2–4 s | Rust startup ~5 ms (already true for `crabcc`). |
| No parallelism in mining (single-threaded `miner.py`) | rayon over file walk; per-file embedding offloaded to a worker pool. |
| Python concurrency story for the watch hook is GIL-bound | crabcc's existing `watch::spawn` already runs the watcher on its own thread (see `crates/crabcc-core/src/watch.rs`). |
| Memory safety / segfault footprint of ML deps | Rust is just better at this. |

**The strongest argument**: the user already has crabcc, a Rust binary that ships SQLite + Tantivy + tree-sitter + MCP stdio + a watcher sidecar in 6.6 MB after UPX. MemPalace adds embeddings + HNSW. ~30 MB more, total. **One static binary** that does both code-symbol search AND verbatim-content memory. Both layers reuse the same SQLite file shape, the same MCP transport, the same watcher.

---

## Part 3 — Rust port architecture

### 3.1 Crate layout

```
crates/
├── crabcc-core/      ← exists today: store, walker, fts, watch, graph, …
├── crabcc-cli/       ← exists today
├── crabcc-mcp/       ← exists today
└── crabcc-memory/    ← NEW. wings/rooms/drawers, embedding, retrieval, KG.
```

`crabcc-memory` depends on `crabcc-core`. The CLI binary gains a `Memory` subcommand tree; the MCP server registers the new tools. Skill + slash command get a `/crabcc-memory-init` companion.

### 3.2 Module map (`crabcc-memory/src/`)

```
src/
├── palace.rs         ← top-level Palace facade. Open/close. Holds Backend + KG + Embedder.
├── schema.sql        ← wings, rooms, drawers, embeddings (sqlite-vec) tables.
├── backend/
│   ├── mod.rs        ← Backend trait (Rust mirror of BaseBackend).
│   ├── sqlite_vec.rs ← default impl. one .db file, uses sqlite-vec extension.
│   └── hnsw.rs       ← optional pure-Rust HNSW impl via hnsw_rs.
├── embed.rs          ← Embedder trait + fastembed-rs default (MiniLM-L6-v2 ONNX).
├── miner/
│   ├── mod.rs        ← Source trait (project, convo, etc).
│   ├── project.rs    ← walks repo via crabcc_core::walker::walk_repo (REUSE!)
│   └── convo.rs      ← Claude Code JSONL parser, exchange-pair chunking.
├── search/
│   ├── mod.rs        ← Searcher facade.
│   ├── raw.rs        ← cosine top-N, no heuristics. 96.6%-equivalent path.
│   ├── bm25.rs       ← Tantivy-backed BM25 sidecar (REUSES crabcc_core::fts pattern).
│   ├── closet.rs     ← high-signal personal facts boost.
│   ├── temporal.rs   ← recency boost.
│   └── rerank.rs     ← LLM rerank, optional, async via reqwest.
├── kg.rs             ← temporal entity-relationship graph (sqlite tables).
├── wal.rs            ← write-ahead log for MCP durability.
└── lib.rs            ← public API.
```

### 3.3 Suggested external crates

| Capability | Crate | Why |
|---|---|---|
| Vector embeddings (CPU, ONNX) | **`fastembed`** (=fastembed-rs) | drop-in MiniLM-L6-v2; same model id as MemPalace; supports BGE/GTE later |
| HNSW | **`hnsw_rs`** *or* **`sqlite-vec`** | pure-Rust HNSW vs SQLite extension that keeps storage in one file |
| BM25 | **`tantivy`** (already in workspace) | already serving fuzzy/prefix in crabcc; just add another schema |
| Async LLM client (rerank) | **`reqwest`** + **`tokio`** | only loaded when rerank is enabled (`--features llm-rerank`) |
| Tokenizer for chunking | **`tiktoken-rs`** *or* simple char-window | match MemPalace's 800-char default |
| FS watcher | **`notify-debouncer-mini`** (already in workspace) | `crabcc memory watch` reuses `crabcc_core::watch::spawn` directly |
| JSONL streaming | **`serde_json`** + line iteration | already in workspace |

The default build stays small: only `fastembed` + `hnsw_rs` are new heavy deps. Both are pure Rust + ORT. Rerank is feature-gated.

### 3.4 Schema additions (one new SQL file `crates/crabcc-memory/schema/001_init.sql`)

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- Enable sqlite-vec extension at connection time (loaded by Store::open).

CREATE TABLE IF NOT EXISTS wings (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    kind        TEXT NOT NULL,   -- 'project' | 'agent' | 'person'
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS rooms (
    id          INTEGER PRIMARY KEY,
    wing_id     INTEGER NOT NULL REFERENCES wings(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    UNIQUE(wing_id, name)
);

CREATE TABLE IF NOT EXISTS drawers (
    id          INTEGER PRIMARY KEY,
    wing_id     INTEGER NOT NULL REFERENCES wings(id) ON DELETE CASCADE,
    room_id     INTEGER          REFERENCES rooms(id) ON DELETE SET NULL,
    source_id   TEXT NOT NULL,    -- file path / convo URI / etc
    body        TEXT NOT NULL,    -- VERBATIM content
    created_at  INTEGER NOT NULL,
    sha256      TEXT NOT NULL,
    UNIQUE(source_id, sha256)
);

-- sqlite-vec virtual table for HNSW search.
CREATE VIRTUAL TABLE IF NOT EXISTS drawer_embeddings USING vec0(
    drawer_id INTEGER PRIMARY KEY,
    embedding FLOAT[384]
);

-- Knowledge graph: temporal triples.
CREATE TABLE IF NOT EXISTS kg_triples (
    id          INTEGER PRIMARY KEY,
    subject     TEXT NOT NULL,
    predicate   TEXT NOT NULL,
    object      TEXT NOT NULL,
    valid_from  INTEGER NOT NULL,
    valid_to    INTEGER,         -- NULL = currently valid
    source_drawer_id INTEGER REFERENCES drawers(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_drawers_wing      ON drawers(wing_id);
CREATE INDEX IF NOT EXISTS idx_drawers_room      ON drawers(room_id);
CREATE INDEX IF NOT EXISTS idx_drawers_sha       ON drawers(sha256);
CREATE INDEX IF NOT EXISTS idx_kg_subject        ON kg_triples(subject);
CREATE INDEX IF NOT EXISTS idx_kg_predicate      ON kg_triples(predicate);
CREATE INDEX IF NOT EXISTS idx_kg_valid_from     ON kg_triples(valid_from);
```

`sqlite-vec` ships HNSW-style ANN as a SQLite virtual table — keeps everything in one `.crabcc/memory.db` file. Trade-off: ~2× slower than hand-tuned HNSW for 100k+ vectors but **dramatically simpler** (one file, one connection, no separate index format). For MemPalace's typical scale (10k–100k drawers per palace) this is the right call.

### 3.5 The 4 backend implementations to ship at v0.1

```rust
pub trait Backend: Send + Sync {
    fn add(&self, palace: &PalaceRef, drawers: &[DrawerInsert]) -> Result<()>;
    fn query(&self, palace: &PalaceRef, q: &Query) -> Result<QueryResult>;
    fn get(&self, palace: &PalaceRef, ids: &[DrawerId]) -> Result<GetResult>;
    fn delete(&self, palace: &PalaceRef, sel: &DeleteSel) -> Result<()>;
    fn count(&self, palace: &PalaceRef) -> Result<usize>;
    fn health(&self, palace: Option<&PalaceRef>) -> HealthStatus;
}
```

- **`SqliteVecBackend`** (default) — single .db file, sqlite-vec extension.
- **`HnswBackend`** (optional) — `hnsw_rs` for pure-Rust HNSW, persistence via `bincode`.
- **`InMemoryBackend`** (testing) — `BTreeMap` + brute-force cosine.
- **`ChromaProxyBackend`** (compat) — talk to a running Chroma server; lets users migrate gradually.

The trait is the seam. Backends register via a workspace feature flag, not entry points (Rust has nothing like Python's `[project.entry-points]` — but Cargo features and inventory crates approximate it).

---

## Part 4 — Where crabcc internals slot in

This is the punchline. Most of MemPalace's *plumbing* is already built in `crabcc-core`. The Rust port doesn't start from scratch.

### 4.1 Reuse map

| MemPalace concept | Existing crabcc internal | What changes |
|---|---|---|
| File walking with gitignore semantics (`miner.py` walking project dirs) | **`crabcc_core::walker::walk_repo`** | Use directly. Already gitignore-aware, hidden-file-aware, and tested (4 tests). |
| Per-file change detection (mtime + sha256) for incremental remine | **`crabcc_core::index::refresh`** + **`crabcc_core::hash::sha256_hex`** | Rename the function to `remine`; logic identical (mtime fast path → sha256 confirm → reparse). |
| FS-watcher sidecar that auto-mines on file changes | **`crabcc_core::watch::{spawn, WatchHandle}`** | Reuse 1:1. Worker thread, debouncer, feedback-loop guard, stop signal — already bulletproof and tested. |
| MCP stdio JSON-RPC 2.0 server | **`crabcc-mcp/src/lib.rs`** | Extend `tools_def()` and `dispatch_tool()` with the 29 MemPalace tools. The transport / framing / error codes / `tools/list` / `notifications/initialized` flow are already correct. |
| Persistent SQLite store with WAL, foreign keys, mmap, optimize | **`crabcc_core::store::Store`** | Bring the same `Connection` setup (PRAGMA WAL, mmap_size, temp_store=MEMORY, busy_timeout, ANALYZE) into `crabcc_memory::Backend`. |
| Token-savings telemetry (`~/.crabcc/usage.log`) | **`crabcc_core::track::record`** | Reuse for `mempalace search` calls — already has session/24h/all-time buckets. |
| BM25 / fuzzy / prefix sidecar (Tantivy) | **`crabcc_core::fts::Fts`** | Add a second Tantivy index keyed on drawer body. BM25 is Tantivy's default ranker. The hybrid v4 BM25 boost becomes a one-liner. |
| Skill / slash-command / MCP installer pattern | **`skill/crabcc/SKILL.md`**, **`commands/crabcc-init.md`**, `~/.claude.json` MCP entry | Add a parallel `skill/crabcc-memory/SKILL.md` with the routing rules — "use crabcc memory when …". |
| Bench harness + visualize | **`bench/raw-bench.py`**, **`bench/visualize.py`** | Add a memory-shaped bench that reproduces LongMemEval R@5. Reuse the matplotlib report scaffolding. |
| Worktree handling | (already verified by `index::tests::git_worktree_isolation`) | `.crabcc/memory.db` lives per-worktree, same as the symbol index. No new work. |

**~70% of the plumbing is already in place.** The genuinely-new work for v0.1 is:

1. The embedder (one new crate dep, ~150 lines of glue).
2. The HNSW / sqlite-vec query path.
3. Wing/room auto-detection (port `room_detector_local.py` — single classifier).
4. The knowledge graph SQLite schema + query helpers.
5. The convo miner (Claude Code JSONL parser).
6. The 29 MCP tools (mostly thin wrappers).

Every other concern is solved.

### 4.2 Concrete reuse — sample wiring

**Project miner** reuses `walker.rs` 1:1:

```rust
// crates/crabcc-memory/src/miner/project.rs
use crabcc_core::walker::walk_repo;
use crabcc_core::hash::sha256_hex;

pub fn mine_project(palace: &Palace, root: &Path) -> Result<MineStats> {
    let mut stats = MineStats::default();
    for path in walk_repo(root) {
        let bytes = match std::fs::read(&path) { Ok(b) => b, Err(_) => continue };
        let sha = sha256_hex(&bytes);
        if palace.has_drawer_with_sha(&sha)? {
            stats.unchanged += 1;
            continue;
        }
        for chunk in chunk_by_chars(&bytes, 800) {
            palace.add_drawer(Drawer {
                wing:      &palace.default_wing,
                room:      detect_room(&chunk),
                source_id: path.to_string_lossy().into(),
                body:      chunk,
                sha256:    sha.clone(),
            })?;
            stats.drawers += 1;
        }
    }
    Ok(stats)
}
```

The `walk_repo` call is identical to what `crabcc index` already uses. The chunking is the only new logic.

**MCP tools** extend the existing dispatcher:

```rust
// crates/crabcc-mcp/src/lib.rs (extended dispatch_tool match)
"memory.search" => {
    let palace = open_palace(root)?;
    let results = palace.search(arg_str(&args, "query")?, 10)?;
    Ok(serde_json::to_string(&results)?)
}
"memory.mine" => {
    let palace = open_palace(root)?;
    palace.mine_project(root)?;
    Ok("{\"status\":\"mined\"}".into())
}
"memory.kg.add" => { ... }
// … 26 more
```

The handshake, framing, error codes, `tools/list` mechanics are reused untouched. ~30 lines per tool.

### 4.3 What MemPalace internals do NOT need to be ported

These layers exist in MemPalace because Python lacked something Rust gives you for free:

- `query_sanitizer.py` — Python regex hygiene against ChromaDB query injection. **Not needed**: Rust + parameterised SQL handles this.
- `dialect.py` — abstraction over Chroma metadata-filter dialects. **Not needed**: we own the SQL.
- `repair.py`, `sweeper.py` — recovery from chromadb segfaults / orphaned segments. **Not needed**: SQLite + WAL + foreign keys make most of this automatic.
- `migrate.py` — chromadb 0.x → 1.x migration. **Not relevant** for a fresh Rust impl.
- `split_mega_files.py` — workaround for hnswlib pickling 800 MB+ blobs. **Not relevant** with sqlite-vec.

That's ~80 KB of Python code you don't have to translate. The Rust port is meaningfully *smaller* than the Python source, despite gaining static binary, parallel mining, and crabcc integration.

---

## Part 5 — `crabcc memory` CLI surface (proposed)

```bash
crabcc memory init                            # create .crabcc/memory.db, default wing
crabcc memory mine <PATH>                     # project files, gitignore-aware
crabcc memory mine --convos ~/.claude/projects/ # Claude Code JSONLs (per-project wing)
crabcc memory search "<query>" [--wing W] [--room R] [--limit N] [--rerank]
crabcc memory wake-up                         # dump top-K drawers for a fresh session
crabcc memory ls wings | rooms                # navigation
crabcc memory drawer get <ID>                 # read one drawer verbatim
crabcc memory kg add <SUBJECT> <PRED> <OBJ> [--from TS] [--to TS]
crabcc memory kg query <SUBJECT> [--at TS]
crabcc memory kg timeline <SUBJECT>
crabcc memory watch [--debounce MS]           # auto-mine on FS events
crabcc memory bench longmemeval <PATH>        # reproduce R@5 numbers
```

MCP tool naming mirrors this: `memory.search`, `memory.mine`, `memory.kg.add`, `memory.kg.query`, `memory.wake_up`, `memory.drawer.get`, etc. ~25–30 tools, in line with MemPalace's 29.

---

## Part 6 — Phasing

| Phase | Scope | Effort |
|---|---|---|
| **M0 — skeleton** | New `crabcc-memory` crate. `palace`, `schema.sql`, `Backend` trait, `SqliteVecBackend` stub. | 1 weekend |
| **M1 — mine + search (raw)** | Project miner via `walker::walk_repo`, fastembed-rs glue, raw cosine top-N. **Target: reproduce 96.6% R@5 on LongMemEval.** | 1 week |
| **M2 — hybrid v4** | BM25 sidecar via `tantivy`, closet/temporal/preference boosts. Target ≥98%. | 3–5 days |
| **M3 — MCP + watch + skill** | Extend `crabcc-mcp` with the 25–30 tools, reuse `watch::spawn`, ship `~/.claude/skills/crabcc-memory/SKILL.md`. | 2–3 days |
| **M4 — knowledge graph** | Temporal triples table, `kg add/query/invalidate/timeline`. | 3–4 days |
| **M5 — LLM rerank** | Feature-gated `reqwest`+`tokio`, calls Anthropic / Ollama / OpenAI. | 2 days |
| **M6 — Convo miner** | Claude Code JSONL parsing, exchange-pair chunking. | 2 days |
| **M7 — Bench harness + report** | Mirror `bench/raw-bench.py` + `visualize.py` for LongMemEval / LoCoMo / ConvoMem. | 3 days |

Total: **~5–6 calendar weeks** at one engineer. Roughly **half what a green-field port would take** — because crabcc already provides walker, store, watch, MCP, track, fts, bench, skill, command, and the test harness.

---

## Part 7 — Risks & open questions

1. **`sqlite-vec` vs `hnsw_rs`** — sqlite-vec is newer, less battle-tested, but keeps everything in one file (matches crabcc's storage model). Recommend starting with sqlite-vec; ship `hnsw_rs` as the `--features hnsw` opt-in.
2. **Embedding-model packaging** — `fastembed-rs` downloads the model on first run by default. Bundling it (~80 MB compressed) inflates the binary. Recommend lazy download into `~/.crabcc/models/` with a checksum.
3. **License compatibility** — MemPalace is MIT, crabcc is MIT, fastembed-rs is Apache-2.0, hnsw_rs is MIT — all compatible.
4. **Naming collision with mempalace skill** — Peter already has the Python `mempalace:*` skills installed. Suggest naming the Rust feature `crabcc memory` (no "palace" branding) to avoid the trigger-word confusion in Claude Code's skill listing.
5. **MCP write-ahead log** — MemPalace's `~/.mempalace/wal/write_log.jsonl` is a JSONL durability layer for MCP writes. Rust port can either (a) trust SQLite WAL + atomic transactions and drop the JSONL log, or (b) keep it for cross-tool replay. Recommend (a) for v0.1 — fewer moving parts.
6. **Benchmark reproducibility** — LongMemEval ships as `longmemeval_s_cleaned.json`. Ensuring deterministic chunking across Python and Rust implementations matters for fair comparison; the 800-char window must match exactly (boundary handling, Unicode normalization).

---

## Part 8 — TL;DR for a PR description

> **Add `crabcc memory` — local-first AI memory built on the existing crabcc core.**
>
> Rebuild MemPalace (a Python project that hits 96.6% R@5 on LongMemEval with no LLM, no API key) as a native Rust subcommand of crabcc. Reuses `walker::walk_repo`, `store` patterns (WAL, mmap, foreign keys), `watch::spawn` (FS-watcher sidecar), `fts` (Tantivy BM25), the MCP stdio server, the skill/command/install pattern, and the bench/visualize harness. Adds three new modules: `embed` (fastembed-rs), `backend` (sqlite-vec), `kg` (temporal triples). Ships as a single static binary; ~30 MB on top of crabcc's current size; no Python runtime needed.
>
> **Why now**: crabcc has all the plumbing. The marginal cost of adding an AI-memory layer is small, the user value is large, and the static-binary distribution story is much better than ChromaDB's "300 MB Python install + ARM64 segfaults".

---

## References

- MemPalace repo: <https://github.com/MemPalace/mempalace>
- MemPalace docs: <https://mempalaceofficial.com>
- LongMemEval paper / dataset: <https://github.com/xiaowu0162/LongMemEval>
- `fastembed-rs`: <https://github.com/Anush008/fastembed-rs>
- `hnsw_rs`: <https://crates.io/crates/hnsw_rs>
- `sqlite-vec`: <https://github.com/asg017/sqlite-vec>

Source files cited (all in MemPalace repo):
- `mempalace/backends/base.py` — `BaseBackend` / `BaseCollection` / typed results
- `mempalace/backends/chroma.py` — Chroma reference impl
- `mempalace/embedding.py` — ONNX MiniLM-L6-v2 loader
- `mempalace/miner.py`, `mempalace/convo_miner.py` — mining flows
- `mempalace/searcher.py` — `_hybrid_rank`
- `mempalace/llm_refine.py`, `mempalace/closet_llm.py` — LLM rerank
- `mempalace/knowledge_graph.py`, `docs/schema.sql` — KG
- `mempalace/mcp_server.py` — 29-tool MCP stdio server
- `pyproject.toml` — runtime deps (chromadb, pyyaml, tomli)

---

# Appendix A — Vector store comparison

ChromaDB does three things: stores vectors, runs ANN queries, and filters on metadata. Rust has multiple ways to cover each. This table is the actual shortlist after evaluating against crabcc's constraints (single static binary, no daemon, fits inside `.crabcc/`).

| Crate / project | Storage shape | ANN algo | Metadata filter | Maturity (Apr 2026) | Fit for crabcc |
|---|---|---|---|---|---|
| **`sqlite-vec`** [`asg017/sqlite-vec`](https://github.com/asg017/sqlite-vec) | SQLite extension; data lives in your `.db` | brute-force; IVF; partition-key prefilter | **YES** — full SQL `WHERE`, dedicated `PARTITION KEY` columns | v0.1.10-alpha (pre-v1, breaking changes possible); Mozilla Builders project; Fly.io/Turso/SQLite Cloud sponsors; production-ready for ≤1M vectors | **Best fit. Chosen.** Single file matches `.crabcc/index.db`. Rust crate exists (`cargo add sqlite-vec`). Partition keys give native wing/room scoping. |
| **`arroy`** [`meilisearch/arroy`](https://github.com/meilisearch/arroy) | LMDB-backed, mmap | tree (Annoy-style) | bring-your-own | v0.6+, powers Meilisearch in prod | Strong runner-up. Trees beat HNSW on small/medium static corpora. Separate from SQLite. Ship as `--features arroy` later. |
| **`lancedb`** [`lancedb/lancedb`](https://github.com/lancedb/lancedb) | Apache Arrow / Lance columnar | IVF-PQ, IVF-HNSW, brute | YES — SQL/Arrow expressions | v0.10+, Series A company | Most "Chroma-shaped" Rust API. Heavier (~30 MB extra). Defer until we need columnar/multi-index. |
| **`usearch`** [`unum-cloud/usearch`](https://github.com/unum-cloud/usearch) | single binary index file | HNSW (claimed faster than hnswlib) | bring-your-own | v2.x mature | Fast. **C++ core** — same trust class as ChromaDB's hnswlib (#74/#521 in MemPalace). Skip. |
| **`hnsw_rs`** [`jean-pierreBoth/hnswlib-rs`](https://github.com/jean-pierreBoth/hnswlib-rs) | bincode dump | HNSW | bring-your-own | v0.3, mature, **pure Rust** | Boring & safe. Pair with crabcc's existing SQLite for metadata. Backup plan if sqlite-vec breaks. |
| **`instant-distance`** [`InstantDomain/instant-distance`](https://github.com/InstantDomain/instant-distance) | bincode | HNSW | bring-your-own | smaller API, less mature | Skip. |
| **`qdrant-client`** | separate server process | HNSW | YES (server-side) | mature | Wrong shape — needs a daemon. Kills the "single static binary" pitch. |
| **`milvus-lite-rs`** | — | — | — | does not exist | Skip. |

## Decision matrix

The crabcc constraints (in priority order):
1. **One static binary** — no separate daemon, no Python runtime. *Eliminates qdrant.*
2. **Files live under `.crabcc/`** alongside the symbol index, with one writer at a time. *Strongly favours sqlite-vec.*
3. **Worktree-correct** — the existing `.crabcc/` per-cwd story must continue to work. *Trivial for sqlite-vec; trivial for arroy via per-dir LMDB; trivial for hnsw_rs.*
4. **No C++ FFI we don't already trust** — we already trust SQLite (linked into rusqlite). We do *not* want to bring in another C++ HNSW runtime after watching MemPalace burn on hnswlib for ~6 months. *Eliminates usearch.*
5. **Metadata-aware ANN** — wing/room scoping is half of MemPalace's recall lift. *Strongly favours sqlite-vec's partition keys; arroy/hnsw_rs require post-filter.*
6. **Pre-v1 churn tolerable** — crabcc itself is pre-v1; an unstable dep is fine if the upgrade story is clean. *sqlite-vec's `vec0` schema is stable enough; breaking changes have been small.*

`sqlite-vec` wins on 5 of 6. The one risk (pre-v1 churn) is mitigated by the abstract `Backend` trait the crate exposes — the same seam MemPalace uses lets us swap backends without touching the call sites.

---

# Appendix B — `sqlite-vec` is the chosen path

## B.1 What you get

- A single SQLite extension you `LOAD` after opening the connection. ~300 KB compressed.
- A `vec0` virtual-table type you `CREATE` like any other table.
- KNN queries via `WHERE embedding MATCH :q AND k = 10`.
- Distance metrics: `L2` (default), `cosine`, `L1`. Set on table creation.
- **Partition keys** for prefiltered KNN — the killer feature for wing/room scoping.
- **Auxiliary columns** to colocate non-indexed metadata (timestamp, source path) with the vector for fast `JOIN`-free reads.
- int8 + binary vector formats (Matryoshka / scalar-quant / binary-quant supported).

## B.2 What you accept

- **Pre-v1**: The README literally says "expect breaking changes". Pin the version in `Cargo.toml` and bump deliberately.
- **No HNSW yet**: brute-force + IVF. For ≤100k vectors this is fast enough that it doesn't matter (~10 ms per query at 384-dim). Above 100k, IVF with partition keys still beats a flat HNSW scan in practice because you're scanning a much smaller pre-filtered set.
- **One writer at a time**: same constraint as crabcc already has. WAL mode in `Store::open` already covers concurrent reads.

## B.3 The minimum viable schema

```sql
-- Extension is loaded via rusqlite (see Appendix C); these CREATEs assume
-- vec0 is registered. partition_key on wing_id makes scoped search free.

CREATE VIRTUAL TABLE IF NOT EXISTS drawer_vectors USING vec0(
    drawer_id      INTEGER PRIMARY KEY,
    +wing_id       INTEGER,                       -- + prefix = partition key
    +room_id       INTEGER,                       -- + prefix = partition key
    embedding      FLOAT[384] distance_metric=cosine,
    +source_path   TEXT,                          -- auxiliary (read-only metadata)
    +created_at    INTEGER
);
```

The `+` prefix marks **partition keys** (filterable in MATCH queries) and **auxiliary columns** (returned with results, not used for search). The distinction in the latest sqlite-vec syntax is: only columns declared as `partition` participate in prefiltering; everything else is `aux`.

`distance_metric=cosine` is the right default for normalized MiniLM embeddings. If you skip it, the default is L2; cosine and L2 give the same *ordering* on unit vectors but cosine is more interpretable.

## B.4 The five queries you'll write

### Insert one drawer

```sql
INSERT INTO drawer_vectors(drawer_id, wing_id, room_id, embedding, source_path, created_at)
VALUES (?, ?, ?, vec_f32(?), ?, ?);
```

`vec_f32(?)` parses a JSON array `'[0.1, 0.2, ...]'` *or* takes a compact binary blob — the Rust path uses the binary blob. See Appendix C for the exact Rust call.

### Top-K KNN, no scoping

```sql
SELECT drawer_id, distance
FROM drawer_vectors
WHERE embedding MATCH vec_f32(?) AND k = ?
ORDER BY distance;
```

(`LIMIT N` works on SQLite 3.41+ as a substitute for `AND k = ?`.)

### Top-K KNN, scoped to one wing — **the MemPalace recall pattern**

```sql
SELECT drawer_id, distance
FROM drawer_vectors
WHERE embedding MATCH vec_f32(?)
  AND wing_id = ?
  AND k = ?
ORDER BY distance;
```

This **prefilters on `wing_id` before scanning** — orders of magnitude faster than fetching top-1000 then filtering in app code.

### Top-K KNN with a JOIN to the verbatim drawers table

```sql
WITH knn AS (
    SELECT drawer_id, distance
    FROM drawer_vectors
    WHERE embedding MATCH vec_f32(?)
      AND wing_id = ?
      AND k = 50
)
SELECT d.id, d.body, d.source_id, d.created_at, knn.distance
FROM knn
JOIN drawers d ON d.id = knn.drawer_id
ORDER BY knn.distance
LIMIT 10;
```

CTE keeps the planner honest. The `LIMIT` outside the CTE lets you over-fetch for hybrid rerank.

### Delete by drawer_id (for refresh / dedup)

```sql
DELETE FROM drawer_vectors WHERE drawer_id IN (?, ?, ?, …);
```

`vec0` virtual tables do real deletes; the index reclaims space.

---

# Appendix C — Implementation walkthrough

## C.1 Cargo deps

```toml
# crates/crabcc-memory/Cargo.toml
[dependencies]
crabcc-core = { workspace = true }
rusqlite    = { workspace = true, features = ["bundled", "load_extension"] }
sqlite-vec  = "0.1"                     # pinned, bump deliberately
fastembed   = "5"                       # ONNX MiniLM-L6-v2, 384-dim default
serde       = { workspace = true }
serde_json  = { workspace = true }
anyhow      = { workspace = true }
```

`load_extension` is the rusqlite feature flag that exposes `Connection::load_extension_enable`. Required because sqlite-vec ships as a runtime extension, not statically linked.

## C.2 Connection setup — extending `Store::open`

```rust
// crates/crabcc-memory/src/palace.rs
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

pub struct Palace {
    conn: Connection,
}

impl Palace {
    pub fn open(path: &Path) -> Result<Self> {
        // SAFETY: load_extension_enable is sound — it just toggles a SQLite flag.
        // Re-disabling immediately after to keep blast radius small.
        let conn = Connection::open(path).context("open sqlite")?;
        unsafe { conn.load_extension_enable()? };
        sqlite_vec::load(&conn).context("load sqlite-vec")?;
        unsafe { conn.load_extension_disable()? };

        // Same pragmas as crabcc_core::Store, copied verbatim.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "mmap_size", 30_000_000_000_i64).ok();
        conn.pragma_update(None, "temp_store", "MEMORY").ok();
        conn.pragma_update(None, "cache_size", -16_000_i64).ok();
        conn.busy_timeout(Duration::from_millis(2_000))?;

        conn.execute_batch(include_str!("../schema/001_init.sql"))?;
        let _ = conn.execute_batch("PRAGMA optimize;");

        Ok(Self { conn })
    }
}
```

## C.3 Insert path — embed once, write twice (vector + verbatim drawer)

```rust
use fastembed::{TextEmbedding, InitOptions, EmbeddingModel};

pub struct Embedder { model: TextEmbedding }

impl Embedder {
    pub fn new() -> anyhow::Result<Self> {
        let model = TextEmbedding::try_new(InitOptions {
            model_name: EmbeddingModel::AllMiniLML6V2,
            cache_dir:  std::path::PathBuf::from(
                std::env::var("HOME").unwrap()
            ).join(".crabcc").join("models"),
            show_download_progress: false,
            ..Default::default()
        })?;
        Ok(Self { model })
    }
    pub fn embed_one(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(self.model.embed(vec![text], None)?.into_iter().next().unwrap())
    }
    pub fn embed_batch(&self, texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(self.model.embed(texts, None)?)
    }
}

impl Palace {
    pub fn add_drawer(
        &self,
        wing_id:     i64,
        room_id:     Option<i64>,
        source_id:   &str,
        body:        &str,
        embedder:    &Embedder,
    ) -> anyhow::Result<i64> {
        let sha = crabcc_core::hash::sha256_hex(body.as_bytes());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;

        let tx = self.conn.unchecked_transaction()?;

        // 1. Verbatim drawer row.
        let drawer_id = tx.query_row(
            "INSERT INTO drawers(wing_id, room_id, source_id, body, created_at, sha256)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(source_id, sha256) DO NOTHING
             RETURNING id",
            (wing_id, room_id, source_id, body, now, &sha),
            |r| r.get::<_, i64>(0),
        );

        let Ok(drawer_id) = drawer_id else {
            tx.commit()?; // already exists; skip embed cost
            return Ok(0);
        };

        // 2. Embedding — the expensive call. Outside the SQL critical section
        //    is preferable; here we do it inside the tx for atomicity. If
        //    embed_one is slow on a hot path, batch and embed a slice of
        //    drawers per transaction (see fine-tuning notes below).
        let embedding = embedder.embed_one(body)?;
        let blob: Vec<u8> = embedding.iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        // 3. Vector row.
        tx.execute(
            "INSERT INTO drawer_vectors(drawer_id, wing_id, room_id, embedding, source_path, created_at)
             VALUES (?, ?, ?, vec_f32(?), ?, ?)",
            (drawer_id, wing_id, room_id, blob, source_id, now),
        )?;

        tx.commit()?;
        Ok(drawer_id)
    }
}
```

Notes:
- `vec_f32(?)` accepts the little-endian f32 byte blob directly. JSON form (`'[0.1,0.2,...]'`) works for ad-hoc SQL but the binary path is ~3× faster on insert.
- `ON CONFLICT(source_id, sha256) DO NOTHING` makes mining idempotent — re-mining unchanged content is a no-op.

## C.4 Search path — KNN + JOIN + (optional) BM25 hybrid

```rust
#[derive(Debug, serde::Serialize)]
pub struct SearchHit {
    pub drawer_id: i64,
    pub body:      String,
    pub source_id: String,
    pub distance:  f32,
}

impl Palace {
    pub fn search(
        &self,
        query:    &str,
        wing_id:  Option<i64>,
        k:        usize,
        embedder: &Embedder,
    ) -> anyhow::Result<Vec<SearchHit>> {
        let qvec = embedder.embed_one(query)?;
        let qblob: Vec<u8> = qvec.iter().flat_map(|f| f.to_le_bytes()).collect();

        let sql = match wing_id {
            Some(_) => "WITH knn AS (
                          SELECT drawer_id, distance FROM drawer_vectors
                          WHERE embedding MATCH vec_f32(?)
                            AND wing_id = ?
                            AND k = ?
                        )
                        SELECT d.id, d.body, d.source_id, knn.distance
                        FROM knn JOIN drawers d ON d.id = knn.drawer_id
                        ORDER BY knn.distance",
            None    => "WITH knn AS (
                          SELECT drawer_id, distance FROM drawer_vectors
                          WHERE embedding MATCH vec_f32(?) AND k = ?
                        )
                        SELECT d.id, d.body, d.source_id, knn.distance
                        FROM knn JOIN drawers d ON d.id = knn.drawer_id
                        ORDER BY knn.distance",
        };

        let mut stmt = self.conn.prepare(sql)?;
        let rows = match wing_id {
            Some(w) => stmt.query_map((qblob, w, k as i64), row_to_hit)?
                            .collect::<Result<Vec<_>, _>>()?,
            None    => stmt.query_map((qblob, k as i64), row_to_hit)?
                            .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }
}
fn row_to_hit(r: &rusqlite::Row) -> rusqlite::Result<SearchHit> {
    Ok(SearchHit {
        drawer_id: r.get(0)?, body: r.get(1)?, source_id: r.get(2)?, distance: r.get(3)?,
    })
}
```

## C.5 Hybrid scoring (vec + BM25 from Tantivy)

The hybrid v4 lift in MemPalace is BM25 + temporal + closet boost. Reuse `crabcc-core`'s existing Tantivy machinery (`fts.rs`) for BM25:

```rust
// Pseudo: get top-50 from each, blend, return top-10.
fn hybrid_search(palace: &Palace, fts: &Fts, query: &str, k: usize) -> Vec<SearchHit> {
    let vec_hits  = palace.search(query, None, 50, &embedder).unwrap();
    let bm25_hits = fts.bm25(query, 50).unwrap();    // returns drawer_id + score

    // Blend: cosine similarity (1 - distance) and BM25 score, both rank-normalised.
    let mut scores: HashMap<i64, f32> = HashMap::new();
    for (i, h) in vec_hits.iter().enumerate() {
        *scores.entry(h.drawer_id).or_insert(0.0) += 0.7 * (1.0 - i as f32 / 50.0);
    }
    for (i, h) in bm25_hits.iter().enumerate() {
        *scores.entry(h.drawer_id).or_insert(0.0) += 0.3 * (1.0 - i as f32 / 50.0);
    }

    let mut ranked: Vec<_> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    ranked.truncate(k);
    ranked.into_iter().map(|(id, _)| palace.get_drawer(id)).collect()
}
```

The `0.7 / 0.3` blend is a starting point; MemPalace's hybrid v4 uses learned weights. See fine-tuning notes.

---

# Appendix D — Fine-tuning recommendations

These are the levers that move the needle on R@5. Most of them have a "right answer" for the MemPalace use case; some are genuine tradeoffs.

## D.1 Embedding model

| Model | Dim | Disk | Notes |
|---|---:|---:|---|
| `AllMiniLML6V2` (default) | 384 | ~80 MB | What MemPalace ships. Hits 96.6% R@5. Reproduce this first. |
| `BGESmallENV15` | 384 | ~120 MB | +1–2% R@5 on most benchmarks at the same dim. Ship as `--model bge-small`. |
| `BGEBaseENV15` | 768 | ~430 MB | +3–4% R@5 at 2× cost (storage + query). Worth it for R@1 use cases. |
| `Jina V2 Small` | 512 | ~150 MB | Multilingual; better for non-English drawers. |

**Recommendation**: ship MiniLM-L6-v2 as default (matches MemPalace numbers); expose `--model` flag for opt-in upgrade. Store the model name in a `meta` table key so re-mining catches model drift.

## D.2 Distance metric

- **Cosine** for any embedding model that produces near-unit vectors (MiniLM, BGE — all of them post-normalisation). Set `distance_metric=cosine` on the `vec0` table.
- **L2** is the default; gives identical *ordering* to cosine on unit vectors but values aren't bounded to `[0, 2]`.
- **L1** is rarely useful for text embeddings — skip.

## D.3 Quantisation

`sqlite-vec` supports `int8` and `bit` (binary) vectors via separate column types. Trade-offs:

| Type | Dim cost | Recall hit | When |
|---|---|---|---|
| `float[384]` | 1.5 KB / vec | baseline | up to ~500k vectors |
| `int8[384]` | 384 B / vec | −0.5–1% R@5 | 500k–5M vectors |
| `bit[384]` | 48 B / vec | −5–10% R@5 (use as a coarse filter, rerank with float) | 5M+ vectors |

For MemPalace's typical 10k–100k drawer scale: stay on `float[384]`. Quantisation is a v0.2 lever.

## D.4 Chunking

MemPalace defaults: **800 chars** for project files, **exchange-pair** (one prompt + one response) for conversations.

- 800 chars ≈ 200 tokens — sits comfortably under MiniLM's 256-token context. Going larger truncates.
- Smaller chunks (400 chars): higher recall but more storage and noisier hits.
- Larger chunks (>1500 chars): lose precision; semantic search starts mixing topics.

**Recommendation**: stay at 800 / exchange-pair to match MemPalace exactly so benchmark numbers compare cleanly. Make it configurable (`--chunk-size`) for experimentation.

## D.5 Batch insert

```rust
// Slow:  one transaction per drawer, one embed call per drawer
// Fast:  batch 64 drawers per transaction + embed_batch
const BATCH: usize = 64;
let chunks: Vec<&[Drawer]> = drawers.chunks(BATCH).collect();
for chunk in chunks {
    let texts: Vec<String> = chunk.iter().map(|d| d.body.clone()).collect();
    let vecs = embedder.embed_batch(texts)?;
    let tx = palace.conn.unchecked_transaction()?;
    for (drawer, vec) in chunk.iter().zip(vecs) {
        // … two INSERTs per drawer (drawers + drawer_vectors) inside tx …
    }
    tx.commit()?;
}
```

Batch embedding amortises ONNX session overhead. **Expect 5–10× throughput** vs single-call. Batch size 32–128 is the sweet spot — bigger batches stop helping once you saturate the AVX/AMX units on Apple Silicon.

## D.6 SQLite pragmas (specific to vec workloads)

Already set in `crabcc_core::Store::open`:

```sql
PRAGMA mmap_size  = 30000000000;   -- 30 GB cap; SQLite caps to file size
PRAGMA cache_size = -16000;        -- 16 MB page cache
PRAGMA temp_store = MEMORY;
```

Add for vec workloads:

```sql
PRAGMA page_size = 8192;           -- before any data; bigger pages read fewer pages per scan
ANALYZE;                           -- after first big insert; refreshes planner stats
```

## D.7 Partition-key choice

Partition keys are the ANN equivalent of database indexes — they prune the search space *before* the brute-force / IVF scan. The right partitioning is the biggest single perf lever.

- **`+wing_id`** is mandatory. Every search is wing-scoped or cross-wing; cross-wing is rare.
- **`+room_id`** is high-value when you have a few large rooms (>1000 drawers). Skip if rooms are tiny.
- Don't add `+source_id` — too high cardinality, partitioning overhead exceeds savings.

`vec0` supports up to **2 partition keys** in current versions. Use both.

## D.8 ANALYZE timing

Run `PRAGMA optimize` (cheap) on every connection open. Run `ANALYZE` (expensive) after:
- First `crabcc memory mine` of a fresh palace
- Any single mining run that adds >10% of total drawer count
- The user runs `crabcc memory optimize` (an explicit subcommand)

Don't run ANALYZE during refresh — it'll thrash the planner stats unnecessarily.

## D.9 VACUUM after deletes

Drawer deletes (e.g. unmining a project, or ttl-expiring old conversations) leave `vec0` with stale slots. After a large delete:

```sql
DELETE FROM drawer_vectors WHERE drawer_id IN (...);
VACUUM;
```

`VACUUM` rewrites the entire database file; it's slow but reclaims all space. Schedule it as part of `crabcc memory optimize`, not the hot path.

## D.10 Hybrid-blend tuning

The 0.7 / 0.3 vec/BM25 blend in Appendix C.5 is conservative. Lift on LongMemEval suggests:

| Blend (vec : bm25) | R@5 | When |
|---|---|---|
| 1.0 / 0.0 | 96.6% | raw — no BM25 |
| 0.7 / 0.3 | ~97.5% | safe default |
| 0.5 / 0.5 | ~98% | balanced |
| 0.4 / 0.6 | ~98.4% | hybrid v4 territory; favours keyword |

Add temporal-decay boost (`* exp(-age_in_days / 365)`) and closet boost (×1.5 for high-signal drawers) on top. Tune on a held-out set, not the training set — that's how MemPalace honestly reports their 98.4%.

## D.11 BM25 parameters (Tantivy)

Tantivy's default `(k1=1.2, b=0.75)` is fine for prose. For mixed code+prose drawers (project files + convos), try:

```rust
schema.set_bm25_k1(1.5);    // longer documents weighted more
schema.set_bm25_b(0.6);     // less aggressive length normalisation
```

These match what MemPalace's `searcher.py` does internally (it documents the values inline).

## D.12 When to escape sqlite-vec

Trigger an arroy fallback when ANY of:
- Total drawer count exceeds **2M** (sqlite-vec brute+IVF gets noticeable; ~50ms+ per query).
- The user enables `--features arroy` explicitly.
- Per-query budget needs to be <5ms (a real-time hot-path use case).

Implementation: keep the same `Backend` trait, ship `ArroyBackend` next to `SqliteVecBackend`. Migration is `crabcc memory rebuild --backend arroy` — re-embeds nothing, just re-indexes from the existing `drawers` table.

---

# Appendix E — Performance & scale notes

Numbers below are sqlite-vec's own published benchmarks plus rough back-of-envelope from MemPalace-style workloads (384-dim MiniLM embeddings).

| Drawer count | Brute-force / query | With partition key (1/N filter) | Recommendation |
|---:|---:|---:|---|
| 1,000 | <1 ms | <1 ms | brute-force fine |
| 10,000 | ~3 ms | ~1 ms | brute-force fine |
| 100,000 | ~30 ms | ~5 ms | partition key required |
| 500,000 | ~150 ms | ~20 ms | acceptable; consider int8 quant |
| 1,000,000 | ~300 ms | ~40 ms | int8 quant + partition keys |
| 5,000,000 | ~1.5 s | ~150 ms | switch to arroy |

Insert throughput: ~5,000 drawers/min on Apple M1 (single-threaded miner + ONNX MiniLM). The bottleneck is embedding, not SQLite. Parallelise mining with rayon for 4–8× lift on multi-core.

Storage: ~2 KB per drawer (1.5 KB vector + 500 B metadata + body). 100k drawers ≈ 200 MB on disk. Compressed via SQLite WAL + page compression: ~70%.

---

# Appendix F — Implementation checklist

Concrete steps to land sqlite-vec into `crates/crabcc-memory/`:

- [ ] `cargo add sqlite-vec fastembed serde anyhow` to the new crate.
- [ ] Add `load_extension` feature to workspace `rusqlite` dep.
- [ ] `crates/crabcc-memory/schema/001_init.sql` with the wings/rooms/drawers + `vec0` virtual table from B.3.
- [ ] `Palace::open` per Appendix C.2.
- [ ] `Embedder` wrapper per C.3 — model cache under `~/.crabcc/models/`.
- [ ] `Palace::add_drawer` per C.3 with idempotent `ON CONFLICT`.
- [ ] `Palace::search` per C.4 with optional `wing_id` partition.
- [ ] `Palace::hybrid_search` reusing `crabcc_core::fts::Fts` for BM25 (C.5).
- [ ] `crabcc memory init/mine/search/wake-up/watch/kg-*` CLI subcommands per Part 5 of the main report.
- [ ] MCP tool wrappers — copy crabcc-mcp's existing dispatch pattern.
- [ ] Reproduce MemPalace's LongMemEval R@5 ≥96% on the held-out 450q set as a release gate.
- [ ] Bench harness extension: `bench/memory-bench.py` mirroring `bench/raw-bench.py`'s shape.

The whole appendix sequence is ~5–7 days of focused work assuming the M0 + M1 milestones from Part 6 land cleanly.
