# PR #648 — perf campaign + agent-shell protector (results)

> Branch `perf/profiling-and-deps-upgrade`. Measurements taken
> **2026-06-04**. Methodology: **measure → optimize → re-bench, never
> blind.** Raw hyperfine artifacts under [`docs/perf-648/`](./perf-648/).

## Bench environment

| | |
|---|---|
| Host | `bench-node` (Hetzner, NixOS) |
| CPU / RAM | 4-core x86_64 / 7.6 GB |
| Toolchain | rustc/cargo 1.95.0 (nix), gcc 15.2.0 |
| Tools | `hyperfine` 1.20, `criterion` 0.8, `samply`, token≈bytes/4 |
| Fixture | the crabcc workspace itself (~437 source files, 4673 symbols, 50970 edges) |

---

## 1. Agent MCP latency — per-session `Store` reuse (headline)

The MCP `serve_io` loop opened a fresh SQLite `Store` per tool call (~680 µs
floor every call). Threading one `Option<Store>` through the session
(`handle_with_session`) opens it once and reuses it. Measured end-to-end by
replaying synthesized agent workloads (`examples/agent_replay.rs`) over stdio
against a spawned `crabcc --mcp`:

| Agent profile | Per-call latency improvement (median) |
|---|---|
| nullclaw   | **~39×** |
| zeroclaw   | **~56×** |
| claude_code | **~45×** |

```
per-call latency (lower is better), normalized to "before = 100"
before  ██████████████████████████████████████████████████ 100
after   █ ~2   (claude_code/nullclaw/zeroclaw, Store-reuse)
```

Supporting micro-opts: `prepare_cached` on the hot `symbol_id_by_name` /
`meta_get` / `upsert_unresolved_sentinel` statements (avoids ~50k statement
recompiles on a cold index), deferred FSST body decode in memory search, and
tree-sitter leaf-cursor guards in the extractor walk.

## 2. `read` tool — schema-ensure + connection cache

The PreToolUse `Read` hook calls `crabcc read` per file; each call re-opened
the memory `Palace` and re-ensured schema.

| Stage | Per-call `read` | claude_code e2e p95 |
|---|---|---|
| before | ~19 ms | 16 ms |
| ensure-schema once/process | ~9 ms | 8 ms |
| + cached session-read connection | **~1 ms** | — |

## 3. Cold full index — parallel parse

`build_index` parsed + extracted one file at a time on a single core.
Profiled split: extract ~37%, sqlite_write ~42%, FTS ~18%. The clean,
low-risk win is parallelising the CPU-bound parse/extract (tree-sitter
parsers are thread-local) and keeping the SQLite write loop serial in
walk order — so file_id assignment + cross-file edge resolution stay
byte-identical (1-thread and 4-thread produce identical counts).

A/B on bench-node (4-core, cold index of the workspace):

| | Mean | Relative |
|---|---|---|
| sequential (pre-change) | 1.408 s ± 0.051 | baseline |
| **parallel (rayon parse)** | **986 ms ± 70** | **1.43× faster** |

```
cold index (lower is better)
sequential  ██████████████████████████████████████████████████ 1.408s
parallel    ███████████████████████████████████▏ 0.986s   (1.43x)
```

Output identical: 441 files / 4700 symbols / 51351 edges. The remaining
SQLite bulk-resolution refactor (~88k per-edge point queries → in-memory
`name→id` map) is a follow-up.

## 4. Global allocator — measured, not guessed

Hypothesis was "mimalloc is a lot faster." Benchmarked the most alloc-heavy
path (cold index), clean prepare, 10 runs each:

| Allocator | Mean | Relative |
|---|---|---|
| system (glibc) | 1.257 s ± 0.023 | **1.00 (fastest)** |
| jemalloc | 1.266 s ± 0.019 | 1.01× |
| mimalloc | 1.292 s ± 0.019 | 1.03× (slowest) |

```
cold index (lower is better)
system    ████████████████████████████████████████ 1.257s
jemalloc  ████████████████████████████████████████▏1.266s
mimalloc  █████████████████████████████████████████ 1.292s
```

**Verdict:** statistical tie (<3%, direction inconsistent across runs). The
allocator is irrelevant for crabcc's hot path on Linux. **Kept jemalloc**
(status quo, what tantivy/tikv ship with); **mimalloc reverted** — no measured
benefit. The in-tree "+5–12% jemalloc" claim did not reproduce.

## 5. Agent-shell protector — rewrite token savings

The PreToolUse Bash hook transparently rewrites provably-equivalent
grep/find to `rg` / `crabcc lookup refs` (see
[`install/hooks-claude.md`](../install/hooks-claude.md)). Output bytes → tokens
(÷4), measured in this worktree (real `target/` + `.gitignore`):

| Original | Rewrite | Tokens (orig → new) | Reduction |
|---|---|---|---|
| `rg Store` | `crabcc lookup refs Store` | 91,432 → 2,062 | **−97.7%** |
| `rg Backend` | `crabcc lookup refs Backend` | 5,609 → 194 | **−96.5%** |
| `rg Rewrite` | `crabcc lookup refs Rewrite` | 590 → 390 | −33.9% |
| `grep -rn TODO crates` | `rg -n TODO crates` | scoped: ~neutral | ~0% |
| `find crates -name '*.rs'` | `rg --files -g '*.rs' crates` | scoped: ~neutral | ~0% |
| `grep -rn Backend .` (built repo) | `crabcc lookup refs Backend` | 8,192⁺ → 194 | **−97.6%** |

⁺ grep over a 25 GB `target/` hit the 60 s cap — true cost is higher.

```
symbol upgrade: rg <ident> -> crabcc lookup refs <ident>
rg        ██████████████████████████████████████████████████ 91.4k tok (rg Store)
lookup    █▏ 2.1k tok
```

**Honest read:** the **symbol upgrade is the reliable structural win**
(34–98%, typically 75–97%) — precise refs instead of every textual match. The
`rg`/`find` swaps save ~0% when the agent already scopes to source; they only
pay off on unscoped `.` searches in repos with large gitignored build dirs.

### Measure / learn loop

Rewrites are logged to `~/.crabcc/_internal.db` (`rewrite_log`, pruned to
~2 MB). A PostToolUse hook measures each rewritten command's actual output; a
symbol upgrade that blows past its token budget (i.e. did **not** reduce
tokens) is flagged `META_ERROR_OPERATOR_NEEDED` and its `(rule, key)`
signature is suppressed, so it passes through unchanged next time. True
pre-exec measurement is impossible (the command hasn't run), so the design is
estimate-gate up front + real measurement + learning post-exec.

## 8. Vanilla Claude Code vs full crabcc flow (end-to-end)

The whole point, one table: a vanilla agent shelling out raw vs the same
intent run through the **full flow** (engine rewrite → RTK → Morph,
auto-engaged from the env). Measured on the crabcc source tree (clean,
no `target/`); tokens ≈ output bytes / 4.

| Agent task | vanilla | full flow | reduction |
|---|---:|---:|---:|
| find symbol (`grep Store` → `lookup refs`) | 88,076 | 1,952 | **−97%** |
| find refs (`grep Backend` → `lookup refs`) | 4,907 | 185 | **−96%** |
| list files (`find *.rs` → `rg --files`+rtk) | 1,736 | 507 | **−70%** |
| read JSON (`cat *.json` → `jq -c`) | 1,149 | 537 | **−53%** |
| text search (`grep 'pub fn'` → rg+rtk+Morph) | 15,509 | 7,192 | **−53%** |
| read source, re-read (`cat store.rs` → `crabcc read`) | 14,790 | 3,715 | **−75%** |
| read source, first read (`cat store.rs` → `crabcc read`) | 14,790 | 15,343 | +4% |

Symbol-aware ops are the standout (−96/−97%: precise refs vs every
textual hit); raw text/file/JSON dumps land −53 to −70% via
gitignore-aware search + RTK + Morph. **Reading source is the one place
we trade Morph for accuracy**: `cat <src>` now rewrites to `crabcc read`,
which serves the *byte-exact* file on the first read (+4% from the JSON
envelope) and a session-cached outline **stub** on every re-read in that
session (−75%), instead of a lossy Morph compaction. Agents re-read the
same files repeatedly across reasoning steps, so the session-amortized
cost drops sharply while accuracy stays perfect (freshness gated on
mtime + content-hash, race-safe via SQLite WAL). Tiny outputs (a handful
of hits) see a small *negative* (~+20 tok) from the provenance header —
the chain is for volume, not trivia. Numbers are flow-invariant to the
underlying crab version (the reductions come from the rewrite layer, not
the index).

## 9. Per-profile matrix (with vs without hooks)

Section 8 is per-*operation*; this is per-*agent-profile* — the same three
CLI-agent usage mixes the MCP benches model (`claude_code` / `nullclaw` /
`zeroclaw`, see `crates/crabcc-mcp/benches/agent_profiles.rs`), each replayed
**vanilla vs through the full flow**. Reproduce with
`task bench-flow-matrix` (or `scripts/bench-flow-matrix.sh`): it benches a
clean `git archive HEAD` tree (no `target/` noise), Morph off, tokens =
bytes/4.

| profile | vanilla | flow | reduction |
|---|---:|---:|---:|
| claude_code (read-heavy, widest mix) | 134,272 | 30,318 | **−77%** |
| nullclaw (lean sequential lookups) | 100,032 | 2,737 | **−97%** |
| zeroclaw (dependency-analysis biased) | 101,332 | 2,800 | **−97%** |

The symbol-lookup-dominated profiles (nullclaw/zeroclaw) collapse to −97%
because nearly every call is a `grep IDENT` → `lookup refs` upgrade. The
read-heavy `claude_code` mix lands −77%: it includes the first-read source
case (+4%) and text search, which dilute the symbol wins. Enabling Morph
(`MORPH_API_KEY` set) pushes `claude_code` further (−81% measured) by
compacting the residual text/source dumps.

**OpenRouter lane (opt-in).** The byte reductions above are
model-independent. To see the *real* billed-token reduction through each
model's own tokenizer, run with `OPENROUTER_API_KEY` + `MODELS="…"` set:
the harness sends a vanilla-context vs flow-context task per model and
reports the API's `usage.prompt_tokens`. Costs real tokens, so it is off by
default and not run here.

## 10. Code RAG — symbol-aware retrieval (`crabcc rag`)

Vector/lexical retrieval over the codebase so an agent can pull the few
snippets relevant to a prompt instead of guessing which files to read.
Built on the existing crabcc-memory `Palace` (FTS5 BM25 ⊕ sqlite-vec ANN,
RRF-fused) — `rag build` chunks at **symbol** granularity (one drawer per
fn/struct/impl, body = signature + source span), which retrieves far
sharper than `memory mine project`'s one-drawer-per-file.

```
crabcc rag build --rebuild     # chunk every indexed symbol (idempotent)
crabcc rag query "QUERY" --limit 8
```

Smoke on this repo: `build` chunked 4,750 symbols / 448 files; `query
"downscale an oversized image to bound vision tokens"` returns
`media.rs::try_downscale` as the top hit. Lexical BM25 by default;
`--features memory-embed` adds semantic MiniLM-L6-v2 hybrid ranking.

**Deliberately not a silent rewrite.** Vector RAG is *fuzzy*; it
complements but never replaces the precise `lookup sym/refs/callers`
surface, so it stays an explicit command. Query results are recorded to
the `crabcc track` ledger (op `rag`) and show up in the dashboard savings
block.

## 7. Which features benefit AI agents most (measured)

Per-operation token cost vs the naive baseline, on the crabcc repo
(bench-node, clean source). Token ≈ bytes/4; latency = median of 5 runs.

| Operation | crabcc | latency | baseline | reduction |
|---|---|---|---|---|
| `sym` (find definition) | 42 tok | 23 ms | grep 92,556 | **−99%** |
| `callers --count` | 3 tok | 22 ms | grep 1,162 | **−99%** |
| `refs` | 2,033 tok | 22 ms | grep 92,556 | **−97%** |
| `outline` (understand a file) | 3,631 tok | 14 ms | cat 14,790 | **−75%** |
| `read` (cache hit → outline stub) | ~3.7k tok | 32 ms | cat re-read 14,790 | **−75%** |
| `files --ext` | 2,142 tok | 14 ms | find 2,327 | −7% |
| `files --ext --group` | dir-keyed | 14 ms | flat array | **−44%** (folds repeated dir prefixes) |

### Top 3 by benefit — human-operated CLI

1. **Symbol query surface — `lookup sym/refs/callers`** (crabcc-core: index
   + store + edges). −97 to −99% tokens. The operator's core questions
   ("where defined / who calls / where used") that otherwise mean grep +
   reading files. Biggest, most-used win.
2. **`outline`** (crabcc-core extract). −75%. The first move on an unfamiliar
   file: structure without dumping the body.
3. **Agent-shell rewrite hook + smart SessionStart context** (crabcc-cli,
   this PR). Unique to interactive use: applies the savings *without the
   operator changing habits* (grep→rg/refs transparently) and points them
   at crabcc up front.

### Top 3 by benefit — agentic (LangChain / MCP, programmatic loop)

1. **MCP server with per-session `Store` reuse** (crabcc-mcp). The delivery
   substrate: exposes every query as a structured JSON tool at 14-32 ms/call
   (per-call DB-open floor removed → ~39-56× e2e). Without it, programmatic
   agents can't use crabcc efficiently; with it, the token wins below reach
   the agent loop as parseable data.
2. **Symbol query tools — `sym/refs/callers`**. Same −97-99% token win, but
   in a loop the per-call token cost is paid every iteration, so the savings
   **compound** across the agent's trajectory.
3. **`read` caching + `outline`** (crabcc-memory `session_reads` +
   crabcc-core). −75%. Agents re-read the same files across reasoning steps;
   the session-keyed outline-stub cache stops them re-dumping full bodies
   every turn. The PostToolUse measure/learn rewrite loop also lives here.

**Module attribution:** crabcc-core (index/store/query/edges) is the engine
behind the top token-savers; crabcc-mcp is the agentic delivery layer;
crabcc-memory powers read-caching. `files`/`find`-style listing is a weak
win (−7%) — gitignore-awareness aside, both just list paths.

## 6. Dependencies + cleanup

Cargo majors bumped (rusqlite 0.39→0.40, reqwest 0.12→0.13 [rustls+webpki],
notify-debouncer-mini 0.4→0.7, ast-grep 0.42→0.43, criterion 0.5→0.8,
tikv-jemallocator 0.6→0.7). Removed the unused `redis`/`bullmq` agent-transport
deps entirely. Fixed a pre-existing dup-edge `UNIQUE` violation
(`INSERT OR IGNORE` against the v4 composite PK) found during validation.
