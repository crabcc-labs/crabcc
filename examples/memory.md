# `crabcc memory` — end-to-end walkthrough

> The full memory layer from issue #2: per-repo `.crabcc/memory.db`,
> hybrid BM25⊕vector search, `mine project` / `mine sessions`
> bulk-ingest, and the `task memory-bench` LongMemEval gate.

This walkthrough takes a fresh repo from "no memory" to "ask
questions across the whole codebase + every conversation I've had
with Claude here". Every command is copy-pasteable; no API keys, no
network calls until you opt into the `memory-embed` model download.

---

## 0 — Install

```bash
cargo install --path crates/crabcc-cli
crabcc --version
```

Or, from a release tarball:

```bash
curl -L https://github.com/peterlodri-sec/crabcc/releases/latest/download/install.sh | sh
```

---

## 1 — Open a memory store

`crabcc memory init` is idempotent — runs the same way against a
fresh repo and a long-lived one. Creates `.crabcc/memory.db` next to
the symbol index.

```bash
$ cd ~/code/my-project
$ crabcc memory init
{"status":"ok","root":"/Users/me/code/my-project"}
```

Health check:

```bash
$ crabcc memory health
"Ok"
```

---

## 2 — Mine the repo (M2 project miner)

`crabcc memory mine project [PATH]` walks the repo via
`crabcc-core`'s ignore-aware `walk_repo` (so `.gitignore`, hidden
dotfiles, and `.crabcc/` itself are skipped) and stores **one drawer
per text file** under `wing="proj"`.

```bash
$ crabcc memory mine project
{"scanned":428,"considered":386,"inserted":386,"deduped":0,"skipped":42}
```

The `skipped` count covers binary files (NUL-byte heuristic on the
first 8 KB), files larger than `--max-bytes` (1 MB by default), and
genuinely empty bodies.

**Idempotent re-run** — change one file, re-mine, only that file
lands as a fresh drawer:

```bash
$ echo "// new line" >> src/lib.rs
$ crabcc memory mine project
{"scanned":428,"considered":386,"inserted":1,"deduped":385,"skipped":42}
```

---

## 3 — Mine your Claude Code sessions (M2 sessions miner)

`crabcc memory mine sessions [DIR]` defaults to
`$HOME/.claude/projects/` — Claude Code's per-conversation JSONL
home. It collapses every `(user, assistant)` turn pair into one
drawer under `wing="session"` so you can later ask
"what did I tell you about X?" across every conversation.

```bash
$ crabcc memory mine sessions
{"scanned":18432,"considered":3120,"inserted":3120,"deduped":0,"skipped":15312}
```

Tool-call / tool-result events are dropped on purpose — they bloat
embeddings without improving recall. The miner also accepts a single
JSONL file path:

```bash
$ crabcc memory mine sessions ~/.claude/projects/my-project/abc-123.jsonl
{"scanned":420,"considered":89,"inserted":89,"deduped":0,"skipped":331}
```

---

## 4 — Search across both wings

The default search is **hybrid** — BM25 (FTS5) ⊕ cosine KNN fused via
Reciprocal Rank Fusion (k = 60). You can ablate to either side via
`--mode lexical` or `--mode vector`.

```bash
# What did I tell Claude about my coffee preferences?
$ crabcc memory search "coffee preferences" --limit 3 --wing session \
    | jq '.hits[] | {source_id, body: (.body[0:80])}'
{
  "source_id": "session:abc-123:7",
  "body": "USER: i actually do all my home coffee on an aeropress with paper filters\n…"
}
…

# Where in the code does the auth refactor live?
$ crabcc memory search "auth refactor" --limit 3 --wing proj \
    | jq '.hits[].source_id'
"proj:src/auth/aurora.rs"
"proj:tests/auth_refactor_test.rs"
"proj:CHANGELOG.md"
```

Cross-wing search (default — no `--wing`):

```bash
$ crabcc memory search "Aurora" --limit 5
```

---

## 5 — Auto-capture symbol queries (optional)

Set `CRABCC_AUTO_MEMORY=1` and every `crabcc sym` / `refs` / `callers`
/ `fuzzy` / `prefix` call quietly stores a drawer summarising the
hit count. Off by default — zero overhead when unset.

```bash
$ export CRABCC_AUTO_MEMORY=1
$ crabcc sym handleAuth >/dev/null
$ crabcc memory list --limit 1 | jq '.[0] | {source_id, body}'
{
  "source_id": "query:sym:handleAuth",
  "body": "sym \"handleAuth\" -> 14 hit(s)"
}
```

---

## 6 — MCP wiring

Every CLI subcommand has a matching `memory.*` MCP tool — 10 in
total: `memory.init`, `memory.remember`, `memory.search`, `memory.get`,
`memory.list`, `memory.delete`, `memory.forget`, `memory.count`,
`memory.health`, `memory.mine_project`, `memory.mine_sessions`.

Wire it into Claude Code:

```bash
claude mcp add crabcc -- crabcc --mcp
```

Each tool accepts an optional `cwd` arg — the server walks up to
`.git` and routes the call to the right per-repo palace, so one
running MCP server can serve drawers from many projects.

---

## 7 — The bench gate (issue #2 R@5 ≥ 96.6%)

`task memory-bench` runs the LongMemEval R@k harness in
[`crates/crabcc-memory-bench/`](../crates/crabcc-memory-bench) against a bundled 12-question
synthetic fixture; expected output:

```
n=12, mode=Lexical, granularity=session, R@1=1.000, R@5=1.000, R@10=1.000
PASS: R@5=1.000 ≥ 0.966
```

To run against the real LongMemEval 450q held-out set:

```bash
mkdir -p crates/crabcc-memory-bench/data
curl -L -o crates/crabcc-memory-bench/data/longmemeval_oracle.json \
  https://huggingface.co/datasets/xiaowu0162/LongMemEval/resolve/main/longmemeval_oracle.json

task memory-bench DATASET=crates/crabcc-memory-bench/data/longmemeval_oracle.json
```

For semantic ranking, build with the `memory-embed` feature
(downloads ~25 MB MiniLM-L6-v2 on first use):

```bash
cargo run --release -p crabcc-memory-bench \
  --features crabcc-memory/memory-embed \
  -- --dataset crates/crabcc-memory-bench/data/longmemeval_oracle.json --mode hybrid
```

---

## 8 — One-screen recap

```bash
# Open + populate
crabcc memory init
crabcc memory mine project
crabcc memory mine sessions

# Use
crabcc memory search "the question I keep forgetting"
crabcc memory list --wing proj --limit 5
crabcc memory count

# Verify the gate
task memory-bench
```
