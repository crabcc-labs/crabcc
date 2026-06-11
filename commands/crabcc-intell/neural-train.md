---
description: Run the ruflo-intelligence RETRIEVE→JUDGE→DISTILL→CONSOLIDATE pipeline on crabcc patterns. Mines session memory, symbol index, and store stats to distill effective lookup strategies and persist them for future sessions.
---

Invoke the `ruflo-intelligence:intelligence-specialist` agent to run a full neural training cycle on crabcc-specific patterns.

## Data sources to mine

### Memory store (`.crabcc/memory.db`)
```bash
crabcc memory list --wing session --limit 200      # recent session drawers
crabcc memory search "lookup" --limit 50           # lookup-shaped memories
crabcc memory search "callers refs sym" --limit 50 # symbol-query patterns
```

### Symbol index (`.crabcc/index.db`)
```bash
crabcc lookup sym Store                     # core entry points
crabcc lookup callers Fts::prefix           # who calls the hot search paths
crabcc lookup callers Codec::decompress     # FSST hot path callers
crabcc lookup outline crates/crabcc-core/src/fts.rs
crabcc lookup outline crates/crabcc-core/src/store.rs
```

### Store index statistics
```bash
sqlite3 .crabcc/index.db \
  "SELECT lang, COUNT(*) AS n FROM symbols JOIN files ON symbols.file_id=files.id GROUP BY lang ORDER BY n DESC;"
```

## Pipeline

1. **RETRIEVE** — run all data-source commands above and collect output.

2. **JUDGE** — score each memory drawer:
   - Led to symbol resolution without grep fallback: +2
   - Pattern matches a currently-indexed symbol: +1
   - References a path no longer in the index (stale): -1

3. **DISTILL** — produce ≤10 rules in this form:
   ```
   RULE: <trigger> → <crabcc command>
   EXAMPLE: camelCase method name → prefix("get") not fuzzy("getUser")
   CONFIDENCE: high|medium|low
   ```

4. **CONSOLIDATE** — persist via crabcc memory and (if available) ruflo IPFS:
   ```bash
   crabcc memory remember "neural-train/lookup-rules" "<distilled rules>"
   crabcc memory remember "neural-train/store-stats"  "<lang distribution>"
   ```
   Then call `memory_store` MCP tool to ship cross-project.

## Spawn instruction

Use the `Agent` tool with `subagent_type: "ruflo-intelligence:intelligence-specialist"` and this prompt:

> Run the crabcc neural training cycle. RETRIEVE: run `crabcc memory list --wing session --limit 200`, `crabcc memory search "lookup" --limit 50`, `crabcc lookup callers Fts::prefix`, `crabcc lookup callers Codec::decompress`, and `sqlite3 .crabcc/index.db "SELECT lang, COUNT(*) FROM symbols JOIN files ON symbols.file_id=files.id GROUP BY lang ORDER BY 2 DESC;"`. JUDGE session patterns for lookup effectiveness. DISTILL into ≤10 rules (format: RULE / EXAMPLE / CONFIDENCE). CONSOLIDATE with `crabcc memory remember "neural-train/$(date +%Y-%m-%d)" "<rules>"` and `memory_store` if available. Return the rules as final output.

## Promoting to AGENTS.md

After review, add high-confidence rules to `AGENTS.md` under:
```
## Learned lookup patterns
<!-- updated by /crabcc-intell:neural-train -->
```
