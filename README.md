<h1 align="center">
  <img src="./assets/logo.svg" alt="crabcc logo" width="160" /><br/>
  crabcc
</h1>

<p align="center">
  <em>Symbol index for AI coding agents.</em><br/>
  <strong>47–5500× faster than <code>grep -rn</code></strong> on monorepos &nbsp;·&nbsp;
  <strong>5–68× faster than ripgrep</strong> on whole-repo lookups &nbsp;·&nbsp;
  <strong>85% fewer bytes</strong> sent to the LLM
</p>

<p align="center">
  <a href="https://github.com/peterlodri-sec/crabcc/actions/workflows/ci.yml">
    <img src="https://img.shields.io/github/actions/workflow/status/peterlodri-sec/crabcc/ci.yml?branch=main&label=CI&style=flat-square" alt="CI status"/>
  </a>
  <a href="https://github.com/peterlodri-sec/crabcc/releases/latest">
    <img src="https://img.shields.io/github/v/release/peterlodri-sec/crabcc?label=release&style=flat-square&include_prereleases" alt="latest release"/>
  </a>
  <a href="https://github.com/peterlodri-sec/crabcc/issues">
    <img src="https://img.shields.io/github/issues/peterlodri-sec/crabcc?style=flat-square" alt="open issues"/>
  </a>
  <img src="https://img.shields.io/badge/rust-1.86%2B-orange?style=flat-square&logo=rust" alt="rust version"/>
  <img src="https://img.shields.io/badge/MCP-server-7057ff?style=flat-square" alt="MCP server"/>
</p>

---

## Install (one line)

```bash
gh api -H 'Accept: application/vnd.github.v3.raw' /repos/peterlodri-sec/crabcc/contents/install.sh | bash
```

That's it. The installer:

- prompts for `gh auth login` if you aren't authenticated yet
- builds `crabcc` from source via `cargo install --locked`
- writes shell completions for your current shell (zsh / bash / fish)
- links the Claude Code skill + slash commands into `~/.claude/` (when
  the `claude` CLI is present)
- prints a `crabcc go` hint so the next thing you do is the right thing

```bash
# follow up with one more line: bootstrap a repo + open a Claude session
cd <your-repo>
crabcc go        # index + graph + memory + claude --effort max --no-chrome
```

Knobs (env or `--flag`):

| flag | env | default | what |
|---|---|---|---|
| `--bin-dir=DIR` | `CRABCC_INSTALL_DIR` | `~/.cargo/bin` | install target |
| `--version=TAG` | — | main HEAD | install a specific release |
| `--no-completions` | — | off | skip shell completions |
| `--no-claude` | — | off | skip ~/.claude/ symlinks |

A small Rust CLI + MCP server that indexes your repo's symbols (functions, classes,
methods, etc.) into a SQLite store and exposes them via four primitives an agent
actually wants: `sym`, `refs`, `callers`, `outline`. Plus token-shaping flags
(`--count`, `--files-only`, `--limit`) that collapse 16k-token result sets to ~3
tokens when the question only needs a number or a deduped file list.

Languages today: TypeScript, TSX, JavaScript, Ruby, Rust, Go, Python. Adding a language
is a tree-sitter grammar plus an extractor.

<p align="center">
  <img src="./assets/demo.gif" alt="crabcc CLI demo against a 13k-file Rails monorepo" width="100%"/>
</p>

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

Deep dives: [`ARCHITECTURE.md`](./ARCHITECTURE.md) · [`docs/RESEARCH-mempalace.md`](./docs/RESEARCH-mempalace.md) · [`docs/RESEARCH-fsst.md`](./docs/RESEARCH-fsst.md) · [`docs/RESEARCH-storage.md`](./docs/RESEARCH-storage.md) · [`examples/`](./examples/) · [`man/crabcc.1`](./man/crabcc.1)
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

```bash
cargo install --path crates/crabcc-cli
crabcc install-claude
```

The interactive `install-claude` subcommand symlinks the skill and slash-command
into `~/.claude/`, then prints the `claude mcp add crabcc -- crabcc --mcp`
invocation and two optional hook snippets (SessionStart auto-refresh,
PreToolUse grep→crabcc hint) for you to paste into `~/.claude/settings.json`.
The subcommand does **not** modify any global Claude config files.

Hook templates: [`install/hooks-claude.json`](./install/hooks-claude.json).
Pass `--yes` to skip the per-symlink prompts, or `--print-hooks` to dump only
the hook JSON to stdout (e.g. `crabcc install-claude --print-hooks > hooks.json`).

Then in Claude Code: `/reload-plugins`.

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
| Store a free-form note for this repo | `crabcc memory remember "doc:1" "<body>"`            |
| Search past notes                    | `crabcc memory search "<query>"`                     |
| List drawers in this repo            | `crabcc memory list --limit 20`                      |

Full examples: [`examples/CLI.md`](./examples/CLI.md). MCP wire-level walkthrough:
[`examples/MCP.md`](./examples/MCP.md).

### AI memory (`crabcc memory`, M0 + M3-light)

Local-first, per-repo memory at `<repo>/.crabcc/memory.db`. M0 ships the
`Backend` trait + a file-backed brute-force `SqliteBackend`; M3-light wires
the CLI/MCP surface (`init`, `remember`, `search`, `get`, `list`,
`delete`, `count`, `health`) plus 8 matching `memory.*` MCP tools. Each
MCP tool accepts an optional `cwd` arg — the server walks up to `.git`
and routes calls to the right per-project palace.

> Note: M0 ships a deterministic hash embedder so the API works
> end-to-end and tests are stable. **Search results are not yet
> semantic** — `fastembed-rs` (MiniLM-L6-v2) lands in M1.

**Auto-capture:** set `CRABCC_AUTO_MEMORY=1` to have `sym` / `refs` /
`callers` / `fuzzy` / `prefix` quietly store a drawer summarising each
query. Off by default (zero overhead).

```bash
CRABCC_AUTO_MEMORY=1 TERM_SESSION_ID="$TERM_SESSION_ID" crabcc sym MyType
crabcc memory list --limit 5    # see what got captured
```

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

<p align="center">
  <img src="./bench/results/savings.png" alt="Bytes per task — crabcc vs ripgrep vs grep" width="100%"/>
</p>
<p align="center">
  <img src="./bench/results/speedup.png" alt="Wall-time speedup — crabcc vs ripgrep vs grep (log scale)" width="100%"/>
</p>

| Task                              | crabcc B | grep B  | crabcc  | grep   | speedup    |
|-----------------------------------|---------:|--------:|--------:|-------:|-----------:|
| `sym User`                        | 1.2k     | 14.1k   | 10.8ms  | 59.3s ⚠| **5493×**  |
| `sym Assessment`                  | 584      | 569     | 11.3ms  | 58.7s ⚠| **5198×**  |
| `callers --count find_by`         | 14       | 9       | 964ms   | 45.0s  | 47×        |
| `refs --files-only Assessment`    | 513      | 460     | 38.1ms  | 13.2s  | 348×       |
| `files --ext rb` (whole repo)     | 244k     | 1.9M    | 13.6ms  | 9.7s   | 713×       |
| `callers --files-only find_by`    | 821      | 831     | 54.3ms  | 45.9s  | **845×**   |

Aggregate: **85% fewer bytes** (≈ 414k input tokens saved per batch, ≈ \$1.24),
**206× faster aggregate wall-time**.

Honest losses: single-file outline of a small file (where `grep -nE` is already
trivial) and small directory listings. crabcc returns rich JSON, raw `grep` returns
just the matching lines — when the question is small, raw wins on bytes.

Full report (with ripgrep comparison): [`bench/results/REPORT.md`](./bench/results/REPORT.md).
Re-run:

```bash
cd bench && python3 raw-bench.py /path/to/your/repo && python3 visualize.py
```

---

## Architecture

```
crates/crabcc-core/   ← extraction, indexing, queries, FTS, tracking
crates/crabcc-cli/    ← clap CLI; Cmd dispatcher
crates/crabcc-mcp/    ← stdio JSON-RPC 2.0 MCP server
crates/crabcc-memory/ ← Palace facade, Backend + Embedder traits, hybrid search
schema/001_init.sql   ← SQLite schema (files, symbols, edges)
skill/crabcc/         ← Claude Code skill (auto-routing rules)
commands/             ← Claude Code slash commands
bench/                ← raw-CLI A/B benchmark harness + visualize
```

### The layers

```
                         ┌───────────────────────────┐
  $ crabcc sym Foo       │ crabcc-cli                │  clap dispatch · sonic-rs
  $ crabcc memory search │   src/main.rs             │  JSON encode · stdout
                         │   src/memory.rs           │
                         └─────────────┬─────────────┘
                                       │
                ┌──────────────────────┼──────────────────────┐
                ▼                      ▼                      ▼
    ┌────────────────────┐ ┌────────────────────┐ ┌────────────────────┐
    │ crabcc-core        │ │ crabcc-memory      │ │ crabcc-mcp         │
    │  walker · extract  │ │  Palace · Backend  │ │  JSON-RPC stdio    │
    │  store · graph     │ │  Embedder · RRF    │ │  tool dispatch     │
    │  track · fts · fsst│ │  PalaceRegistry    │ │  OpenAPI 3.1 spec  │
    └─────────┬──────────┘ └──────────┬─────────┘ └─────────┬──────────┘
              │                       │                     │
              └───────────────────────┼─────────────────────┘
                                      ▼
                       ┌──────────────────────────────┐
                       │ Per-repo state at .crabcc/   │
                       │   index.db (FTS5 + symbols)  │
                       │   tantivy/ (fuzzy + prefix)  │
                       │   graph.json (call graph)    │
                       │   memory.db (drawers + vec)  │
                       │   fsst.symbols (codec)       │
                       └──────────────────────────────┘
```

The CLI is a thin dispatcher: clap parses, the matched arm calls into one of three library crates, and `sonic_rs::to_string` encodes the result. The library crates are independent — `crabcc-mcp` runs the same code paths as the CLI but over JSON-RPC 2.0 instead of argv. `crabcc-memory` is the only crate that needs `.crabcc/memory.db`; everything else lives in `index.db`.

### Per-command mechanics

- **Indexing**: ignore-walks the repo, runs tree-sitter per file, extracts symbols via per-language rules in `extract.rs`, persists to SQLite.
- **`sym`**: SQL lookup, `WHERE name = ?`. Microseconds.
- **`refs`**: enumerate indexed files, `memchr` prefilter on the byte needle, walk tree-sitter to find identifier nodes equal to the name. Early-stops on `--limit`.
- **`callers`**: same as refs but uses ast-grep patterns `name($$$)` and `$RECV.name($$$)` to also catch method-receiver calls. Or, on indexes with edges populated, a single SQL scan over the `edges` table (O(callers), not O(files)).
- **`outline`**: SQL `WHERE file_id = ? ORDER BY line_start`.
- **`files`**: SQL on the indexed-files table, optionally filtered by prefix/lang/ext.
- **`fuzzy` / `prefix`**: Tantivy sidecar at `.crabcc/tantivy/`. Rebuilt automatically on `crabcc index`; explicit `crabcc fts-rebuild` for refresh-only flows.
- **`memory search`**: hybrid by default — vector cosine KNN + FTS5 BM25 fused via Reciprocal Rank Fusion (k = 60). `--mode lexical` or `--mode vector` to ablate.
- **`track`**: appends a JSONL log to `~/.crabcc/usage.log`, summarized by `crabcc track`.
- **`watch`**: notify-debouncer-mini-based FS watcher on its own thread. Auto-runs `refresh` on file changes. Feedback-loop guard skips events under `.crabcc/`.
- **`graph`**: call-graph sidecar persisted at `.crabcc/graph.json`, built from the `edges` table populated at extract time (one SQL scan, O(files); v1.0.0's symbols × files walker is the fallback for un-reindexed DBs). Subcommands: `build`, `walk NAME [--dir callers|callees] [--depth N]`, `cycles`, `orphans`.

### Trace: what happens internally during a CLI call

#### `crabcc sym Foo` — point lookup

```text
clap parses argv ─►  Cmd::Sym { name: "Foo", root, compress }
                                    │
                     Store::open(.crabcc/index.db)  ─►  WAL · mmap · pragmas
                                    │
                     SELECT id, name, kind, signature, parent,
                            file, line_start, line_end, visibility
                            FROM symbols WHERE name = ?  ─►  rusqlite prepared stmt
                                    │
                     [optional]  Codec::decompress(signature)
                                    │      (only if FSST is on AND row is encoded)
                                    │
                     sonic_rs::to_string(&Vec<Symbol>)  ─►  ~hundreds of µs
                                    │
                     println!("{body}")                 ─►  stdout
```

Cost is dominated by the SQLite open and the prepared-statement execute; the JSON encode is in the noise. Repeated `sym` calls in the same MCP session reuse the cached `Arc<Palace>` (and its `Connection`) via `PalaceRegistry`.

#### `crabcc callers find_by --count` — token-shaped output

```text
clap parses ─► Cmd::Callers { name: "find_by", mode: Count }
                  │
                  query::query_callers(store, "find_by", Mode::Count)
                  │
                  ┌─ if `edges` table populated and non-empty:
                  │    SELECT COUNT(*) FROM edges WHERE dst_name = ?
                  │    ─► single SQL aggregate, sub-millisecond
                  │
                  └─ else (legacy index):
                       ast-grep patterns `find_by($$$)` and `$R.find_by($$$)`
                       over every indexed file → count matches
                       ─► O(files) walk; still fast on 13k-file repos
                  │
                  println!("{count}")  ─►  14 bytes total
```

The `--count` shape is what makes this 5500× faster than `grep -rn` on big monorepos: we never touch source bytes, we just hit a single integer in SQLite.

#### `crabcc refs UserId --files-only --limit 5` — early-stop walk

```text
clap parses ─► Cmd::Refs { name: "UserId", mode: FilesOnly { limit: 5 } }
                  │
                  store.list_indexed_files()  ─►  cheap; SQL projection
                  │
                  for file in indexed_files:
                      bytes  = read(file)
                      hits   = memchr(name.as_bytes(), &bytes)   ◄── prefilter
                      if no hits: continue
                      tree   = parser.parse(&bytes)
                      for node in tree.walk():
                          if node.kind == identifier && text == "UserId":
                              push_dedup(file)
                              if files.len() == limit: return early   ◄── early stop
                  │
                  sonic_rs::to_string(&files)  ─►  ~few hundred bytes
                  │
                  println!
```

`memchr` short-circuits any file that doesn't contain the byte sequence at all, so we only pay the tree-sitter parse for files that might match. The early stop on `--limit` lets agents narrow scope cheaply: `--files-only --limit 5` typically scans a few percent of the repo.

#### `crabcc memory search "the fox"` — hybrid retrieval

```text
clap parses ─► memory::Cmd::Search { query, limit, wing, room, mode }
                  │
                  Palace::open(repo_root)  ─►  .crabcc/memory.db + Embedder + Backend
                  │
                  ┌─────────────────────────────┴─────────────────────────────┐
                  ▼                                                           ▼
        Embedder::embed_one(query)                              Backend::query_lexical(LexicalQuery)
        │  HashEmbedder by default                              │  FTS5 MATCH on `drawers_fts`
        │  FastEmbedder if --features memory-embed              │  BM25 ranking
        │  CachedEmbedder wraps either: sha256(text) → cached    │  returns Vec<DrawerHit> (lexical)
        │  Vec<f32> hits skip the inner embedder
        ▼
        Backend::query(Query { embedding, limit, … })
        │  brute-force cosine over `drawers.embedding` blob
        │  (or sqlite-vec ANN with --features memory-vec)
        │  returns Vec<DrawerHit> (vector)
                  │                                                           │
                  └────────────────────────────┬──────────────────────────────┘
                                               ▼
                                rrf_fuse(&[vector_hits, lexical_hits], limit)
                                  │  k = 60 (Cormack/Clarke/Buettcher 2009)
                                  │  contribution = 1 / (k + rank)
                                  │  hits in both rankings stack ─►  top-K
                                  ▼
                                sonic_rs::to_string(&QueryResult)
                                  │
                                  println!
```

The two rankers run independently against the same `.crabcc/memory.db`; RRF fuses ranks (not raw scores), which is why hybrid out-performs either ranker alone without needing per-corpus score normalization.

For deeper architectural detail, mermaid diagrams of the data flow and threading model, and runbooks for adding features, see [`ARCHITECTURE.md`](./ARCHITECTURE.md).

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

v2.0.0 + v2.1.0 shipped (2026-04-30): edges-at-extract makes `graph build`
O(files) and caller queries pure SQL; FSST string compression on signatures
+ memory drawer bodies; `crabcc memory` M0 + M3-light surface (CLI + 8
`memory.*` MCP tools); v2.1.0 adds `crabcc upgrade` + shell completions.
Languages: TypeScript / TSX / JavaScript / Ruby / Rust / Go / Python.
**130+ tests** across the workspace. License: MIT.
CI matrix: Linux x86_64 + Linux aarch64 + macOS aarch64. (Intel macOS
dropped in v1.0.1 — `cargo install` from source.)

### Roadmap

| Milestone | Status | Tracked |
|---|---|---|
| Token-shaping flags + `crabcc files` | ✅ shipped (v1.0.0) | — |
| Watch + Graph sidecars | ✅ shipped (v1.0.0) | — |
| SQLite tuning + +14 coverage tests | ✅ shipped (v1.0.0) | — |
| CI: nextest + JUnit XML artifact | ✅ shipped (v1.0.0) | — |
| ARCHITECTURE.md + install.sh + brew skeleton | ✅ shipped (v1.0.0) | — |
| Languages: Go, Python, Rust | ✅ shipped (v1.1.0) | [#4](https://github.com/peterlodri-sec/crabcc/issues/4) |
| CI optimizations (sccache, smarter cache) | ✅ shipped | [#6](https://github.com/peterlodri-sec/crabcc/issues/6) |
| FSST string compression | ✅ shipped (v2.0.0) | [#1](https://github.com/peterlodri-sec/crabcc/issues/1) |
| Edges-at-extract (graph build O(n²)→O(n)) | ✅ shipped (v2.0.0) | [#3](https://github.com/peterlodri-sec/crabcc/issues/3) |
| `crabcc memory` M0 + M3-light surface | ✅ shipped (v2.0.0) | [#2](https://github.com/peterlodri-sec/crabcc/issues/2) |
| `crabcc upgrade` + shell completions | ✅ shipped (v2.1.0) | — |
| **Memory: semantic search (M0.5 + M1)** | 🚧 v2.5 sprint 1 | [#2](https://github.com/peterlodri-sec/crabcc/issues/2) |
| **Distribution: brew tap, mdBook, demos** | 🚧 v2.5 sprint 2 | [#5](https://github.com/peterlodri-sec/crabcc/issues/5) |

Full v2.5 plan: [`docs/ROADMAP-v2.5.md`](./docs/ROADMAP-v2.5.md).
