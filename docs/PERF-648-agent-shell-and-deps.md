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

## 3. Cold full index

Deps bump + extractor leaf-guards. Fresh reconfirm (jemalloc, HEAD index path):

| | Mean | Notes |
|---|---|---|
| cold `crabcc index` | **1.32 s ± 0.10** | ~437 files; was ~1.48 s pre-campaign (~11%) |

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

## 6. Dependencies + cleanup

Cargo majors bumped (rusqlite 0.39→0.40, reqwest 0.12→0.13 [rustls+webpki],
notify-debouncer-mini 0.4→0.7, ast-grep 0.42→0.43, criterion 0.5→0.8,
tikv-jemallocator 0.6→0.7). Removed the unused `redis`/`bullmq` agent-transport
deps entirely. Fixed a pre-existing dup-edge `UNIQUE` violation
(`INSERT OR IGNORE` against the v4 composite PK) found during validation.
