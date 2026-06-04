# crabcc tool offerings

This inventory maps crabcc's current documented tools to the common CLI tools,
agent plugins, and shell workflows they replace or complement.

Source basis: the visible workspace docs in this artifact (`README.md`,
`examples/*.md`, `man/crabcc.1`, `CHANGELOG.md`). `SESSION-HANDOFF.xml` mentions
additional inflight `watch` and `graph` work, but those source files are not
present in this workspace copy, so they are listed separately as unverified.

## Recommendation on fzf and zoxide

| Candidate | Fit? | Existing overlap | Recommendation |
|---|---|---|---|
| `fzf` >= 0.53.0 | Partial fit for human interactive browsing. Poor fit as a core agent dependency. | `crabcc fuzzy` and `crabcc prefix` already provide non-interactive fuzzy/prefix symbol lookup. `crabcc files` already lists indexed code files. | Do not make it a core dependency. Add optional recipes like the ones below for humans. |
| `zoxide` | Low fit for crabcc core. | No crabcc equivalent tracks historical directories. crabcc already has explicit roots via cwd or `--root`; MCP configs can pin repo roots. | Do not add as a dependency. It is useful in a user's shell, not inside crabcc. Optional wrapper idea only: pick a repo with `zoxide query -i`, then run `crabcc --root ...`. |

Short version: `fzf` is a nice optional UI layer over crabcc output. `zoxide`
solves shell directory jumping, which is adjacent but outside crabcc's code
lookup mission.

## Current documented tools

| crabcc tool | Replaces or reduces | Best use | Notes |
|---|---|---|---|
| `crabcc sym NAME` | Definition regex scans with `grep` or `rg` | Exact symbol definition lookup | Returns structured `{name, kind, signature, parent, file, line_start, line_end, visibility}`. |
| `crabcc refs NAME` | `grep -rn NAME`, `rg '\bNAME\b'` for identifier references | Full identifier-reference hits | AST/identifier-aware enough to avoid treating every raw string as equivalent. |
| `crabcc refs NAME --files-only` | `rg -l`, `grep -rl`, manual dedupe | "Which files reference X?" | Token-shaped output avoids dumping snippets. |
| `crabcc refs NAME --count` | `rg` plus `wc -l`, agent-side counting | "How many references?" | Emits only `{"count":N}`. |
| `crabcc callers NAME` | Call-site regex scans with `grep` or `rg` | Call-site discovery | Catches bare calls and receiver calls. |
| `crabcc callers NAME --files-only` | `rg -l` plus regex and dedupe | "Which files call X?" | Usually the right first pass for impact analysis. |
| `crabcc callers NAME --count` | Shell count pipelines | Call-count checks | Docs note `rg` can be competitive for tight count-only regexes. |
| `crabcc outline FILE` | Reading or dumping a whole file to understand shape | "What's in this file?" | Returns symbols and line ranges without method bodies. Use the ranges for selective reads. |
| `crabcc files` | `ls -R`, `find . -name`, some `rg --files` code-file listings | Indexed code-file listing | SQLite-backed, gitignore-aware, filterable by `--under`, `--lang`, `--ext`, `--limit`. |
| `crabcc fuzzy QUERY` | Guess-and-grep for misspelled symbol names | Typo-tolerant symbol lookup | Native, token-aware Levenshtein-distance-2 over symbol names, read live from the index. |
| `crabcc prefix QUERY` | Prefix regex scans over definitions | Starts-with symbol lookup | Case-insensitive, token-aware prefix search over symbol names. |
| `crabcc index` | Repeated cold disk scans | Initial repo indexing | Builds `.crabcc/index.db` (fuzzy/prefix read it directly — no sidecar). |
| `crabcc refresh` | Rebuilding from scratch after edits | Incremental index update | mtime + sha256 keyed; fast no-op path. |
| `crabcc fts-rebuild` | — | Retained no-op (back-compat) | Fuzzy/prefix read the live index, so there is no sidecar to rebuild; just reports the symbol count. |
| `crabcc track` | Ad hoc token-saving estimates | Usage and savings telemetry | Estimates tokens saved versus `grep + Read`. |
| `crabcc --mcp` | Repeated CLI subprocess calls by agents | Persistent agent tool server | Exposes the same lookup primitives through MCP `tools/call`. |

## MCP tools currently advertised

The visible MCP docs advertise 9 tools:

| MCP tool | CLI equivalent | Replaces |
|---|---|---|
| `sym` | `crabcc sym` | Definition grep. |
| `refs` | `crabcc refs` | Reference grep, file-only grep, count pipelines. |
| `callers` | `crabcc callers` | Call-site regex grep. |
| `outline` | `crabcc outline` | Whole-file reads for structure. |
| `files` | `crabcc files` | `ls -R`, `find`, code-file `rg --files`. |
| `index` | `crabcc index` | Manual index setup. |
| `refresh` | `crabcc refresh` | Manual rebuild after edits. |
| `fuzzy` | `crabcc fuzzy` | Guess-and-grep symbol search. |
| `prefix` | `crabcc prefix` | Prefix definition scans. |

## Companion tools crabcc should keep, not replace

| Tool | Why keep it |
|---|---|
| `rg` / ripgrep | Best for free text in markdown, YAML, JSON, configs, comments, strings, or one small file where raw lines are enough. |
| `fd` | Best for filename globbing, age-based file queries, non-code files, and pre-index situations. |
| `jq` | Best for reshaping crabcc JSON into exactly the fields needed downstream. |
| `sed` / editor read-range tools | Best after `sym` or `outline` gives a precise line range and the caller needs source bodies. |
| `fzf` | Best as an optional human TUI over `crabcc files`, `crabcc fuzzy`, or `crabcc prefix` output. |
| `zoxide` | Best in the user's shell for historical directory jumping before invoking crabcc. |

## Agent/plugin replacements

| Existing agent behavior | crabcc replacement |
|---|---|
| Bash `grep -rn` across a repo | `sym`, `refs`, or `callers`, choosing `--count` / `--files-only` / `--limit` when possible. |
| Bash `find . -name` or `ls -R` for code files | `files` with `--under`, `--lang`, `--ext`, and `--limit`. |
| Blind `Read` of a large file to understand structure | `outline`, then selective read of only the returned line ranges. |
| Repeated CLI subprocess calls from an agent | `crabcc --mcp` so the agent uses persistent typed tools. |
| Manual routing rules remembered by the user | crabcc skill and `/crabcc-init` command, when installed, to teach the agent when to pick crabcc versus `rg`/`fd`/`jq`. |

## Optional fzf recipes

These are good documentation snippets, not core product requirements:

```bash
# Pick an indexed code file interactively.
crabcc files | jq -r '.[]' | fzf

# Pick a symbol candidate from prefix results.
crabcc prefix getUser | jq -r '.[] | "\(.name)\t\(.kind)\t\(.file):\(.line)"' | fzf

# Pick a fuzzy match, then inspect its definition separately.
crabcc fuzzy Asseessment | jq -r '.[] | "\(.name)\t\(.kind)\t\(.file):\(.line)"' | fzf
```

If these become official, document them under examples instead of adding `fzf` to
the Rust binary's dependency surface.

## Handoff-mentioned but not verified in this workspace copy

`SESSION-HANDOFF.xml` describes inflight features that are not present in the
visible files in this task workspace:

| Tool | Claimed purpose | Replacement category |
|---|---|---|
| `crabcc watch` | FS watchdog sidecar for auto-refresh after file changes | Replaces manual `crabcc refresh` loops. |
| `crabcc graph-build` | Build `.crabcc/graph.json` call graph sidecar | Precomputes graph navigation. |
| `crabcc graph NAME --dir callers/callees --depth N` | Traverse callers/callees via BFS | Replaces ad hoc repeated caller/callee searches. |
| MCP `graph` | Agent-facing graph traversal tool | Replaces multiple round trips for call graph exploration. |

Before documenting those as current product surface, verify against the actual
source tree and installed binary.
