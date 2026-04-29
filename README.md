<h1 align="center">
  <img src="./assets/logo.svg" alt="crabcc logo" width="160" /><br/>
  crabcc
</h1>

<p align="center">
  <em>Symbol index for AI coding agents.</em><br/>
  <strong>47–4400× faster than <code>grep -rn</code></strong> on monorepos &nbsp;·&nbsp;
  <strong>5–100× faster than ripgrep</strong> on whole-repo lookups &nbsp;·&nbsp;
  <strong>85% fewer bytes</strong> sent to the LLM
</p>

<p align="center">
  <a href="https://github.com/peterlodri-sec/crabcc/actions/workflows/ci.yml">
    <img src="https://img.shields.io/github/actions/workflow/status/peterlodri-sec/crabcc/ci.yml?branch=main&label=CI&style=flat-square" alt="CI status"/>
  </a>
  <a href="https://github.com/peterlodri-sec/crabcc/releases/latest">
    <img src="https://img.shields.io/github/v/release/peterlodri-sec/crabcc?label=release&style=flat-square&include_prereleases" alt="latest release"/>
  </a>
  <a href="https://github.com/peterlodri-sec/crabcc/blob/main/LICENSE">
    <img src="https://img.shields.io/github/license/peterlodri-sec/crabcc?style=flat-square" alt="MIT licensed"/>
  </a>
  <a href="https://github.com/peterlodri-sec/crabcc/stargazers">
    <img src="https://img.shields.io/github/stars/peterlodri-sec/crabcc?style=flat-square" alt="stars"/>
  </a>
  <a href="https://github.com/peterlodri-sec/crabcc/issues">
    <img src="https://img.shields.io/github/issues/peterlodri-sec/crabcc?style=flat-square" alt="open issues"/>
  </a>
  <a href="https://github.com/peterlodri-sec/crabcc/milestone/1">
    <img src="https://img.shields.io/github/milestones/progress/peterlodri-sec/crabcc/1?label=v2.0&style=flat-square" alt="v2.0 progress"/>
  </a>
  <img src="https://img.shields.io/badge/rust-1.86%2B-orange?style=flat-square&logo=rust" alt="rust version"/>
  <img src="https://img.shields.io/badge/MCP-server-7057ff?style=flat-square" alt="MCP server"/>
</p>

---

A small Rust CLI + MCP server that indexes your repo's symbols (functions, classes,
methods, etc.) into a SQLite store and exposes them via four primitives an agent
actually wants: `sym`, `refs`, `callers`, `outline`. Plus token-shaping flags
(`--count`, `--files-only`, `--limit`) that collapse 16k-token result sets to ~3
tokens when the question only needs a number or a deduped file list.

Languages today: TypeScript, TSX, JavaScript, Ruby. Adding a language is a tree-sitter
grammar plus an extractor.

```text
$ crabcc sym Assessment
[{"name":"Assessment","kind":"class","signature":"class Assessment < ApplicationRecord",
  "file":"app/models/assessment.rb","line_start":1,"line_end":991, ... }]

$ crabcc callers find_by --count
{"count":475}

$ crabcc refs Assessment --files-only --limit 10
{"files":["app/builders/.../part_builder.rb", ...]}      ← 253 bytes vs 62,541
                                                          (–99.6%)
```

<details>
<summary>📚 Table of contents</summary>

- [Why](#why)
- [Install](#install)
- [Usage](#usage)
- [Token-shaping flags](#token-shaping-flags)
- [Bench results](#bench-results-mc-mothership-13k-indexed-files)
- [Architecture](#architecture)
- [When NOT to use crabcc](#when-not-to-use-crabcc)
- [Status & roadmap](#status)

Deep dives: [`ARCHITECTURE.md`](./ARCHITECTURE.md) · [`docs/RESEARCH-mempalace.md`](./docs/RESEARCH-mempalace.md) · [`docs/RESEARCH-fsst.md`](./docs/RESEARCH-fsst.md) · [`examples/`](./examples/) · [`man/crabcc.1`](./man/crabcc.1)
</details>

---

## Why

`grep -rn` and `find . -name` are the wrong defaults for an LLM. They walk
`node_modules/`, `.git/`, `tmp/`. They emit unstructured text the agent has to
re-parse. They don't understand "symbol" — `class User` and the string `"User"` look
the same to grep. And on a real monorepo (~13k files), they routinely **time out at
60 seconds**.

`rg`/`fd` fix the gitignore part but still rescan from disk on every query. crabcc
reads from a SQLite index — the answer is already in memory. Plus the output is
typed: `{name, kind, signature, parent, file, line_start, line_end}`, not a wall of
text.

---

## Install

```bash
# One-liner (Linux + macOS, x86_64 + aarch64)
curl -fsSL https://raw.githubusercontent.com/peterlodri-sec/crabcc/main/install.sh | bash

# Or from source
cargo install --path crates/crabcc-cli
```

```bash
crabcc index            # one-time, ~5–30s on a 13k-file repo
crabcc refresh          # incremental, ~250ms no-op (mtime + sha256 keyed)
crabcc watch            # auto-refresh on file changes (Ctrl-C to exit)
```

The index lives at `.crabcc/index.db` per repo. Add `.crabcc/` to `.gitignore`.

### Claude Code integration

`crabcc` ships as an MCP server, a skill, and a slash command. To install all three globally:

```bash
# MCP — exposes 9 tools to Claude Code (sym/refs/callers/outline/files/index/refresh/fuzzy/prefix)
# Add to ~/.claude.json under "mcpServers":
#   "crabcc": { "command": "crabcc", "args": ["--mcp"] }

# Skill — auto-loads the routing rules
ln -s "$(pwd)/skill/crabcc/SKILL.md" ~/.claude/skills/crabcc/SKILL.md

# Command — /crabcc-init slash command
ln -s "$(pwd)/commands/crabcc-init.md" ~/.claude/commands/crabcc-init.md
```

Then `/reload-plugins` in Claude Code.

---

## Usage

| Question                             | Command                                              |
|--------------------------------------|------------------------------------------------------|
| Where is `Foo` defined?              | `crabcc sym Foo`                                     |
| What calls `handleAuth`?             | `crabcc callers handleAuth`                          |
| **How many** call sites of `find_by`?| `crabcc callers find_by --count`                     |
| **Which files** reference `UserId`?  | `crabcc refs UserId --files-only --limit 20`         |
| All references to `UserId`           | `crabcc refs UserId`                                 |
| What's in this file?                 | `crabcc outline path/to/file.rb`                     |
| List `.rb` files under `app/models`  | `crabcc files --under app/models --ext rb`           |
| Misremembered name?                  | `crabcc fuzzy Asseessment`  (Levenshtein dist 2)     |
| Names starting with…                 | `crabcc prefix getUser`                              |
| How many tokens have I saved?        | `crabcc track`                                       |

Full examples: [`examples/CLI.md`](./examples/CLI.md). MCP wire-level walkthrough:
[`examples/MCP.md`](./examples/MCP.md).

---

## Token-shaping flags

`refs` and `callers` accept three mutually-exclusive output shapes:

```bash
crabcc refs Assessment                       # 62,541 bytes — full hits
crabcc refs Assessment --files-only --limit 5    # 253 bytes  (–99.6%)
crabcc refs Assessment --count                   # 14 bytes   (–99.98%)
```

Pick the smallest shape the question allows. The early-stop on `--limit` makes the
small-shape calls cheaper at the CLI layer too — not just smaller payload, fewer
files walked.

Pair with `jq` for projection:

```bash
crabcc outline foo.rb | jq -r '.[] | [.name, .line_start] | @tsv'
crabcc callers find_by | jq 'group_by(.file) | map({file: .[0].file, n: length})'
```

---

## Bench results (mc-mothership, ~13k indexed files)

CLI-vs-CLI, no Claude session involved. Measures only the bytes the LLM's stdout
buffer would receive and wall-time.

| Task                    | crabcc B | grep B    | crabcc | grep   | speedup |
|-------------------------|---------:|----------:|-------:|-------:|--------:|
| `sym User`              | 1.2k     | TIMEOUT⚠   | 68ms   | 60s ⚠  | **884×**|
| `sym Assessment`        | 584      | TIMEOUT⚠   | 61ms   | 60s ⚠  | **982×**|
| `callers --count find_by`| 14       | 9         | 1.06s  | 48.9s  | 46×     |
| `refs --files-only Assessment` | 513 | 460     | 32ms   | 14.0s  | 436×    |
| `files --ext rb` (whole repo)  | 244k| 1.9M     | 14ms   | 10.4s  | 757×    |
| `callers --files-only find_by` | 821 | 831     | 56ms   | 47.0s  | **841×**|

Aggregate: **85% fewer bytes** (≈ 411k input tokens saved per batch), **182× faster
aggregate wall-time**.

Honest losses: single-file outline of a small file (where `grep -nE` is already
trivial) and small directory listings. crabcc returns rich JSON, raw `grep` returns
just the matching lines — when the question is small, raw wins on bytes.

Full report: [`bench/results/REPORT.md`](./bench/results/REPORT.md). Re-run:

```bash
cd bench && python3 raw-bench.py /path/to/your/repo && python3 visualize.py
```

---

## Architecture

```
crates/crabcc-core/   ← extraction, indexing, queries, FTS, tracking
crates/crabcc-cli/    ← clap CLI; Cmd dispatcher
crates/crabcc-mcp/    ← stdio JSON-RPC 2.0 MCP server
schema/001_init.sql   ← SQLite schema (files, symbols, edges)
skill/crabcc/         ← Claude Code skill (auto-routing rules)
commands/             ← Claude Code slash commands
bench/                ← raw-CLI A/B benchmark harness + visualize
```

- **Indexing**: ignore-walks the repo, runs tree-sitter per file, extracts symbols
  via per-language rules in `extract.rs`, persists to SQLite.
- **`sym`**: SQL lookup, `WHERE name = ?`. Microseconds.
- **`refs`**: enumerate indexed files, `memchr` prefilter on the byte needle, walk
  tree-sitter to find identifier nodes equal to the name. Early-stops on `--limit`.
- **`callers`**: same as refs but uses ast-grep patterns `name($$$)` and
  `$RECV.name($$$)` to also catch method-receiver calls.
- **`outline`**: SQL `WHERE file_id = ? ORDER BY line_start`.
- **`files`**: SQL on the indexed-files table, optionally filtered by prefix/lang/ext.
- **`fuzzy` / `prefix`**: Tantivy sidecar at `.crabcc/tantivy/`. Rebuilt automatically
  on `crabcc index`; explicit `crabcc fts-rebuild` for refresh-only flows.
- **`track`**: appends a JSONL log to `~/.crabcc/usage.log`, summarized by `crabcc track`.
- **`watch`**: notify-debouncer-mini-based FS watcher running on its own thread.
  Auto-runs `refresh` on file changes. Feedback-loop guard skips events under `.crabcc/`.
- **`graph`**: call-graph BFS sidecar persisted at `.crabcc/graph.json`. Build once with
  `crabcc graph-build`, query with `crabcc graph NAME [--dir callers|callees] [--depth N]`.

For deeper architectural detail, mermaid diagrams of the data flow and threading model,
and runbooks for adding features, see [`ARCHITECTURE.md`](./ARCHITECTURE.md).

---

## When NOT to use crabcc

| Situation                                     | Reach for                              |
|-----------------------------------------------|----------------------------------------|
| Free-text in markdown / yaml / json / configs | `rg "pattern" path/`                   |
| Need full function bodies                     | `crabcc sym X`, then `Read` line range |
| Filename glob / age / non-code files          | `fd PATTERN path/`                     |
| Repo isn't indexed yet                        | `crabcc index` (or `rg`/`fd` for now)  |
| Single small file, raw lines                  | `rg -n pattern file` is already cheap  |

**Never reach for `grep -rn` or `find . -name`** on a real repo.

---

## Status

v0.1 → v2.0 in flight. Languages: TS/TSX/JS/Ruby (Go/Python/Rust queued for v2.0).
**102 tests** (85 core + 17 MCP). License: MIT. CI matrix: Linux + macOS x86_64 + macOS aarch64.

### Roadmap

| Milestone | Status | Tracked |
|---|---|---|
| Token-shaping flags + `crabcc files` | ✅ shipped | — |
| Watch + Graph sidecars | ✅ shipped | — |
| SQLite tuning + +14 coverage tests | ✅ shipped | — |
| CI: nextest + JUnit XML artifact | ✅ shipped | — |
| ARCHITECTURE.md + install.sh + brew formula | ✅ shipped | — |
| **`crabcc memory` MVP** (MemPalace port) | 📋 v2.0 | [#2](https://github.com/peterlodri-sec/crabcc/issues/2) |
| **Edges-at-extract** (graph build O(n²)→O(n)) | 📋 v2.0 | [#3](https://github.com/peterlodri-sec/crabcc/issues/3) |
| **Languages: Go, Python, Rust** | 📋 v2.0 | [#4](https://github.com/peterlodri-sec/crabcc/issues/4) |
| **Distribution: brew tap, mdBook, demos** | 📋 v2.0 | [#5](https://github.com/peterlodri-sec/crabcc/issues/5) |
| **CI optimizations** (sccache, smarter cache) | 📋 v2.0 | [#6](https://github.com/peterlodri-sec/crabcc/issues/6) |
| **FSST string compression** | 📋 v2.0 | [#1](https://github.com/peterlodri-sec/crabcc/issues/1) |

Sprint plan: [`task-items/crabcc/.tasks`](../../task-items/crabcc/.tasks) (4-dev × 2-week).
Full v2.0 milestone: <https://github.com/peterlodri-sec/crabcc/milestone/1>.
