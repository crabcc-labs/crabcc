# 5.1.0 performance workstream — tantivy + LSP

Goal: make tantivy access/usage and the `ucracc-lsp` hot paths blazingly fast.
Medium-effort, landed incrementally. **Measure → optimize → re-bench**, never blind.

## Measure

- **Live**: the `ucracc.stats` command + `~/.crabcc/ucracc-lsp-stats.json` shutdown
  dump + `ucracc_lsp::stats` tracing events (PR #624) give per-method
  count/errors/avg/max latency from real sessions.
- **Benches**: `crates/ucracc-lsp/benches/{baseline_vs_lsp,symbols,extractor_cost}.rs`
  and core `task bench-compress`. Gate each change against these.

## Bottlenecks & wins (priority order)

1. **[LANDED, this PR] FTS reader reuse.** `Fts::exec` rebuilt a fresh
   `IndexReader` on every `fuzzy`/`prefix` call (re-opening segment readers).
   Now built once in `Fts::open` and reused; `rebuild` calls `reader.reload()`.
   Touches the `crabcc fuzzy/prefix` CLI/MCP path and the LSP `symbol()` handler.
2. **LSP store-lock serialization (headline).** Every `hover`/`goto_definition`/
   `references`/`symbol` locks the single `Arc<Mutex<Option<Store>>>` for the whole
   query, so all LSP reads serialize on one mutex. Win: a small pool of read-only
   rusqlite connections (or per-thread `Store`) so concurrent requests run in
   parallel. `Store` is `!Sync`, so this needs a connection-pool wrapper. (M-L)
3. **Whole-cache invalidation on every keystroke.** `did_open`/`did_change` call
   `self.cache.invalidate_all()`, so the moka query cache is nuked on every edit —
   the hover/definition right after a keystroke always cold-misses. Win: scope
   invalidation to the changed file's symbols/keys. (S-M, big felt latency win)
4. **`symbol()` N+1 re-hydration.** For each FTS prefix hit it re-queries
   `find_symbol(store, hit.name)` (one SQLite round-trip per hit). The FTS hit
   already carries name/file/line/kind/parent — build `SymbolInformation`
   directly, skipping the re-query. (S)
5. **Prefix regex reuse.** `prefix` rebuilds a `RegexQuery` per call; cache the
   compiled pattern for repeated prefixes (editor sends one per keystroke). (S)

## Validation

Each item: bench before/after with the harnesses above; check `ucracc.stats`
avg/max latency on a real repo (mc-mothership ~38k symbols) drops. No regressions
to cold-start (< 100 ms) or hover p95 (< 20 ms) targets.

## Status

- #624 — stats (the measurement layer). Merged-pending.
- This PR — win #1 (FTS reader reuse).
- #2–#5 — follow-on PRs, each bench-gated.
