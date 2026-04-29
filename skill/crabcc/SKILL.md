---
name: crabcc
description: Use crabcc CLI for code lookups instead of grep/find/ls/Read on whole files. Triggers when looking up a symbol definition, finding callers/references, listing top-level structure of a file, or doing symbol-aware search. Skip for free-text content searches in non-code files.
---

# crabcc — symbol index for code lookups

A `crabcc` CLI binary is installed at the repo root via `.crabcc/index.db`.
Prefer it over raw `grep`, `rg`, `find`, or `Read` for code-shape questions.
It returns compact JSON tuned for token efficiency.

## When to use

- "Where is `Foo` defined?" → `crabcc sym Foo`
- "What calls `handleAuth`?" → `crabcc callers handleAuth`
- "Show all references to `UserId`" → `crabcc refs UserId`
- "What's in this file?" → `crabcc outline path/to/file.rs`
- "Find usages of pattern X in code" → `crabcc grep X`

## When NOT to use

- Text content in markdown / config / data files → use `rg` or `Read`.
- You need full function bodies → after `crabcc sym`, `Read` the exact line range it returns.
- Repo isn't indexed (`.crabcc/index.db` missing) → run `/crabcc-init` first.

## Output shape

All commands print one JSON line per result. Fields are `name, kind, signature, parent, file, line_start, line_end`.
Pipe through `jq` only if the user asks — raw JSON is already compact.

## Re-indexing

If lookups return stale results, run `crabcc refresh` (incremental, hash-keyed).
A full rebuild is `crabcc index`.

## Token-cost rule of thumb

A `crabcc sym` call returning 3 hits costs ~150 tokens.
The equivalent `grep -rn` + 3× `Read` costs ~3,000–8,000 tokens.
Default to `crabcc` for code-shape questions.
