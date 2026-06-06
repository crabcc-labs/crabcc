---
name: crabcc
description: Use crabcc CLI for code lookups instead of grep/find/ls/Read on whole files. Triggers when looking up a symbol definition, finding callers/references, listing files in a directory, listing top-level structure of a file, or doing symbol-aware search. Skip for free-text content searches in non-code files (use rg/fd instead).
---

# crabcc — symbol index for code lookups

A `crabcc` CLI binary is installed (`~/.cargo/bin/crabcc`). Each repo gets its own
index at `.crabcc/index.db`. Prefer crabcc over raw `grep`, `rg`, `find`, `fd`, `ls`,
or `Read` for code-shape questions. It returns compact JSON tuned for token efficiency.

**Bench data (mc-mothership, 13k files): 47–4400× faster than `grep -rn`, 85% fewer
bytes in aggregate.**

## Tool ladder — pick the right tool for the question

```
              question shape
┌──────────────────────────────────────────────┬────────────────────────────────┐
│ "where is symbol Foo?"                       │ crabcc sym Foo                 │
│ "who calls Foo?" / "what references Foo?"    │ crabcc callers / refs Foo      │
│ "what's in this code file?"                  │ crabcc outline file.rb         │
│ "list code files under X / by ext"           │ crabcc files --under X --ext rb│
├──────────────────────────────────────────────┼────────────────────────────────┤
│ "free text in code" (regex literal, error,   │ rg "pattern" path/             │
│  config-ish content, log lines, TODO scan)   │   (NEVER plain `grep -rn`)     │
│ "find filenames by glob/extension/age"       │ fd PATTERN path/               │
│   when you don't have a crabcc index, or     │   (NEVER plain `find . -name`) │
│   the files aren't code (yaml, md, json, …)  │                                │
├──────────────────────────────────────────────┼────────────────────────────────┤
│ "filter / project crabcc JSON output"        │ crabcc … | jq …                │
└──────────────────────────────────────────────┴────────────────────────────────┘
```

**Never reach for `grep -rn` or `find . -name` on a real repo.** They walk
`node_modules/`, `.git/`, `tmp/`, etc. — slow and noisy. `rg` and `fd` are
gitignore-aware by default, like crabcc.

## When to use crabcc

| Question                          | Command                                              |
|-----------------------------------|------------------------------------------------------|
| "Where is `Foo` defined?"         | `crabcc sym Foo`                                     |
| "What calls `handleAuth`?"        | `crabcc callers handleAuth`                          |
| "How many call sites of X?"       | `crabcc callers X --count`                           |
| "Which files reference `UserId`?" | `crabcc refs UserId --files-only --limit 20`         |
| "All references to `UserId`"      | `crabcc refs UserId`                                 |
| "What's in this file?"            | `crabcc outline path/to/file.rs`                     |
| "List `.rb` files in dir"         | `crabcc files --under app/models --ext rb`           |
| "Misremembered name"              | `crabcc fuzzy Asseessment`  (Levenshtein dist 2)     |
| "Names starting with…"            | `crabcc prefix getUser`                              |
| "How much have I saved?"          | `crabcc track`                                       |

## Token-shaping flags (refs / callers)

These shape the output before it hits the agent's context. **Use them whenever the
question doesn't actually need the full hit list:**

- `--count` → `{"count": N}` only. Replaces 16k-token result sets with ~3 tokens.
- `--files-only [--limit N]` → deduped JSON file list. ~88% smaller than full hits.
- `--limit N` → cap the full hit list. Early-stops the per-file walk.

Pick the smallest shape the question allows. "How many?" → `--count`. "Which files?"
→ `--files-only`. "Show me a few examples" → `--limit 5`.

## Pair with `jq` for advanced shaping

crabcc emits compact JSON; `jq` projects it into exactly what you need:

```bash
# Just the file paths from a refs query
crabcc refs Assessment | jq -r '.[].file' | sort -u

# Symbol name + line, tab-separated (for quick "open this" lists)
crabcc outline app/models/user.rb | jq -r '.[] | [.name, .line_start] | @tsv'

# Find every public method in a class
crabcc outline foo.rb | jq '[.[] | select(.kind=="method" and .visibility!="private")]'

# Callers grouped by file with a count
crabcc callers find_by | jq 'group_by(.file) | map({file: .[0].file, n: length})'
```

`jq -r` outputs raw strings (no JSON quoting) — usually what you want when piping to
another tool or showing the user a clean list.

## When NOT to use crabcc — use `rg`, `fd`, or `Read` instead

| Situation                                     | Reach for                                     |
|-----------------------------------------------|-----------------------------------------------|
| Free-text content in markdown / yaml / json   | `rg "pattern" path/`                          |
| Need full function bodies                     | `crabcc sym X`, then `Read` the line range    |
| Filename glob / extension / non-code file     | `fd PATTERN path/` (or crabcc files for code) |
| Repo isn't indexed (`.crabcc/index.db` gone)  | run `/crabcc-init` first, or fall back to `rg`|
| Single file < 200 lines, you want raw lines   | `rg -n pattern file` is already cheap         |
| Search across binary content / non-text       | `rg --binary` or `Read`                       |

`rg` and `fd` are direct replacements for `grep` and `find` and are bundled with
Claude Code on most setups — use them by default. Plain `grep -rn` and `find . -name`
should be considered deprecated for repo work.

> **Big-file throughput tip:** for `awk` / `sort` / `grep` / `rg` over multi-GB
> *ASCII* files, prefix `LC_ALL=C` (e.g. `LC_ALL=C awk '{print $1}' huge.log`) —
> it skips per-character UTF-8 classification for a 2–10× speedup. Drop it when the
> data is genuinely multibyte: the C locale changes collation, regex character
> classes (`[[:alpha:]]`), and case folding, so it's only safe for byte/ASCII work.

## Output shape

All commands print compact JSON to stdout. Symbols include `{name, kind, signature,
parent, file, line_start, line_end, visibility}`. Hits include `{file, line, col,
snippet}`. Pipe through `jq` whenever you need to reshape — see examples above.

## Re-indexing

- `crabcc refresh` → incremental, mtime + sha256 keyed (~250ms no-op on 13k files).
- `crabcc index` → full rebuild of the SQLite symbol index.
- `crabcc fts-rebuild` → no-op, kept for back-compat (fuzzy/prefix read the live
  index directly now, so there is no separate sidecar to rebuild).

Fuzzy/prefix always reflect the current `index.db`, so they're never stale on
their own. If lookups return stale results, run `refresh` first.

## Token-cost rule of thumb

| Operation                         | crabcc cost  | rg + Read cost     | grep + Read cost   |
|-----------------------------------|-------------:|-------------------:|-------------------:|
| `sym User` (1 def)                | ~150 tok     | ~500–1,500 tok     | ~3,000–8,000 tok   |
| `callers find_by --count`         | ~3 tok       | ~200–800 tok       | ~1,000–4,000 tok   |
| `refs Assessment --files-only`    | ~480 tok     | ~1,800 tok         | ~15,000 tok        |
| `outline foo.rb` (991 lines)      | ~4,400 tok   | n/a                | ~30,000 tok (Read) |
| `files --ext rb`                  | ~60k tok     | n/a (use fd)       | ~470k tok (find)   |

Default to crabcc for code-shape questions; fall back to `rg`/`fd` for everything else.
The CLI is so much faster than `grep -rn` on monorepos (47–4400×) that even when bytes
are similar, time-to-answer wins.

## See also — skills built on top of `crabcc`

- [`skill/warp-speed-audit/SKILL.md`](../warp-speed-audit/SKILL.md) — audits a
  Rust repo against the [jFransham warp-speed gist](https://gist.github.com/jFransham/369a86eff00e5f280ed25121454acec1)
  using `crabcc` + `repomix` + 4 parallel sub-agents. Tracked in
  [issue #84](https://github.com/peterlodri-sec/crabcc/issues/84). Every
  lookup it performs is a `--count` / `--files-only` / `--limit`-shaped
  variant from this file's tool-ladder.
