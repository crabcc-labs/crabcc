---
name: warp-speed-audit
description: Audit a Rust repo against the 14 tips from jFransham's "Achieving warp speed with Rust" gist (https://gist.github.com/jFransham/369a86eff00e5f280ed25121454acec1). Triggers on "/warp-speed-audit <path>", "warp speed audit", "audit rust perf", "find warp-speed opportunities", "scan for warp speed tips", or any request to find applications of that specific gist on a Rust codebase. Uses crabcc for symbol-aware lookups, repomix for hot-crate packing, and parallel sub-agents. Read-only; produces `<repo>/.crabcc/warp-speed-report.md`.
---

# warp-speed-audit — apply the jFransham gist to a Rust repo

> Tracking issue: **https://github.com/peterlodri-sec/crabcc/issues/84**
> Background research: attached as the first comment on issue #84
> ([direct link](https://github.com/peterlodri-sec/crabcc/issues/84#issuecomment-4350276107)).
> Companion skill: [`skill/crabcc/SKILL.md`](../crabcc/SKILL.md) — every lookup
> below is the symbol-aware variant explained there. Read its tool-ladder
> first; this skill assumes it.

This file is **self-contained** — sub-agents need only the prompt their
orchestrator writes from this skill plus a working `crabcc` binary.

Maps the 14 actionable tips from
[Achieving warp speed with Rust](https://gist.github.com/jFransham/369a86eff00e5f280ed25121454acec1)
onto detectable patterns in a target Rust codebase, using **crabcc** (symbol
index), **repomix** (hot-crate packing), and **4 parallel sub-agents**.

**Inputs**:  a repo path (defaults to `pwd` when invoked as `/warp-speed-audit`).
**Output**:  `<repo>/.crabcc/warp-speed-report.md`.
**Side effects**:  creates `<repo>/.crabcc/index.db` if missing.
**Read-only** to source code.

---

## When this skill fires

- `/warp-speed-audit [path]`.
- The user asks about "warp speed", "jFransham gist", "Rust perf audit", or
  pastes the gist URL.
- The user explicitly asks to find applications of issue #84 on a repo.

If `<path>` has no `Cargo.toml` (root or `crates/*/Cargo.toml`), stop and tell
the user. Do not run on non-Rust repos.

---

## Phase 0 — ENV/PATH bootstrap (run ONCE, share with every sub-agent)

Sub-agents inherit the parent's process env but **not the interactive shell's
rc-files**. Any `~/.cargo/bin` that lives only in `~/.zshrc` will be missing.
Resolve binaries up front and pass them as **environment variables** in each
sub-agent prompt.

```bash
# 1. Resolve binaries (allow overrides; fall back to PATH; finally ~/.cargo).
export REPO_PATH="${1:-$PWD}"
export CRABCC_BIN="${CRABCC_BIN:-$(command -v crabcc || echo "$HOME/.cargo/bin/crabcc")}"
export REPOMIX_BIN="${REPOMIX_BIN:-$(command -v repomix || echo "npx --yes repomix")}"

# 2. Hard-fail if crabcc isn't there. The audit is meaningless without it.
test -x "$CRABCC_BIN" || { echo "crabcc not found — install via cargo install crabcc"; exit 1; }

# 3. Make sure $HOME/.cargo/bin is on PATH for any nested shells the agents spawn.
case ":$PATH:" in *":$HOME/.cargo/bin:"*) ;; *) export PATH="$HOME/.cargo/bin:$PATH" ;; esac

# 4. Index the repo if needed (skip if .crabcc/index.db is fresh).
cd "$REPO_PATH"
test -f .crabcc/index.db || "$CRABCC_BIN" index
"$CRABCC_BIN" refresh   # idempotent, ~250 ms on 13k files
```

Every sub-agent prompt MUST include these four lines in the briefing:

```
REPO_PATH=<absolute path>
CRABCC_BIN=<absolute path or "crabcc">
REPOMIX_BIN=<absolute path or "npx --yes repomix">
PATH already augmented with $HOME/.cargo/bin
```

This avoids each agent re-running `command -v` and prevents the most common
failure mode (silent fall-through to `grep`).

---

## Phase 1 — Cargo.toml fast-path (synchronous, before fan-out)

These are highest-impact, lowest-effort findings — don't waste an agent on
them. Read `<repo>/Cargo.toml` and `crates/*/Cargo.toml` directly:

| Tip | Check                                                        | Cost |
|-----|--------------------------------------------------------------|------|
| 10  | `[profile.release]` → `lto` (none/`false`/`"thin"`/`"fat"`)  | 1 read |
| 10  | `[profile.release]` → `codegen-units` (>1 → soft-flag)       | 1 read |
| 10  | `[profile.release]` → `panic = "abort"` (binary crates)      | 1 read |
| 7   | Workspace deps include `smallvec`/`arrayvec`/`tinyvec`?      | 1 read |
| 12  | Workspace deps include `rayon`?                              | 1 read |
| 13  | Workspace deps include `memchr`/`bytecount`?                 | 1 read |

Findings go straight into the report's "Highest-impact wins" section. The
fan-out agents will not re-check these.

---

## Phase 2 — Parallel fan-out (4 sub-agents via local Ollama)

The orchestrator (this skill) runs all `crabcc` probes itself, then fans
out the **analysis** of those probe outputs to four parallel local
Ollama calls via `scripts/ollama-fanout.sh`. This replaces Claude
Code's `Agent` tool fan-out — free, local, Metal-accelerated on Apple
Silicon, no per-call token cost.

### Backend setup (once per host)

```bash
task system-check          # verify RAM / disk / arch / daemon
task ollama-bootstrap      # install Ollama + pull the OpenClaw model
task ollama-smoke          # confirm fan-out script returns merged JSON
```

Default model: **`voytas26/openclaw-oss-20b-deterministic`** — gpt-oss
20B fine-tune for OpenClaw autonomous agents ("strict tool schema
adherence, concise outputs, stable behavior in automation workflows").
Needs ~13 GB disk + ~16 GB RAM. Override via
`CRABCC_OLLAMA_MODEL=…` (see `task ollama-bootstrap` for alternatives).

### Step 1 — orchestrator runs probes

For each cluster, the orchestrator runs the `crabcc` commands listed in
the per-agent sections below and captures the JSON output. Each cluster
gets its own `/tmp/warp-probe-<cluster>.json` artifact.

### Step 2 — build prompts + fan out

Build `/tmp/warp-prompts.json` as a JSON array of four
`{name, prompt}` objects (one per cluster A/B/C/D). Each prompt
contains:

- The gist URL and the tip ids the cluster owns.
- The cluster's probe JSON (paste raw — model parses it).
- The **JSON output contract** below (verbatim).
- The **False-positive policy** below (verbatim).
- Hard rule: "respond with valid JSON only, no commentary".

Then:

```bash
bash scripts/ollama-fanout.sh \
  --prompts /tmp/warp-prompts.json \
  --output  /tmp/warp-replies.json \
  --json-mode \
  --parallel 4
```

`--json-mode` sets Ollama's `format: "json"` so the response field is
strict JSON. The script's `--parallel 4` matches the cluster count.
Per-cluster timeout is 600 s (10 min); the 20B model usually returns
in < 60 s on M-series.

### Step 3 — parse + aggregate

Read `/tmp/warp-replies.json`. Each entry has `{name, ok, response,
…}`. The `response` field is the model's JSON string — `jq -r
'.[] | .response | fromjson'` extracts findings. Aggregate per the JSON
output contract.

If any cluster has `ok: false`, surface the error in the report's
"Skipped tips" section but continue with the other three.

### Token discipline (still required)

The Ollama backend is free, but model context windows aren't. Keep
prompts lean:

- Always run `--count` first; skip the cluster if zero hits.
- Cap `crabcc` output with `--files-only` / `--limit 30`.
- Don't paste whole files into the prompt; use `crabcc outline <file>`
  line ranges and let the model decide if it wants more (it can't ask,
  so be conservative).

### Optional fallback — Claude Code Agent tool

If `task system-check` returns FAIL or Ollama is unreachable, the skill
MAY fall back to Claude Code's `Agent` tool with the same per-cluster
prompts. Default to Ollama; only fall back when the user explicitly
asks.

Cap each report to ≤ 300 words; cap each crabcc call's output with
`--count` / `--files-only` / `--limit 30`.

#### JSON output contract (every sub-agent returns exactly this shape)

```json
{
  "agent": "A | B | C | D",
  "tip_ids": [6, 11],
  "env_echo": {
    "REPO_PATH": "...",
    "CRABCC_BIN": "...",
    "REPOMIX_BIN": "..."
  },
  "findings": [
    {
      "tip_id": 6,
      "tip": "Avoid Box<dyn Trait>",
      "file": "crates/foo/src/bar.rs",
      "line": 42,
      "severity": "low | medium | high",
      "snippet": "fn do_x(p: Box<dyn Parser>) -> ...",
      "suggestion": "Switch to `&mut dyn Parser` or generic `P: Parser`."
    }
  ],
  "skipped_because": null
}
```

If the agent's first probe returns 0 hits, return `{"findings": [],
"skipped_because": "<reason>"}` — don't pad.

#### False-positive policy (every sub-agent must apply)

- **Tip 11 — `#[inline(always)]`**: only flag when fn body > 30 lines
  (use `crabcc outline <file>` line range). Tiny accessors are legitimate.
- **Tip 6 — `Box<dyn Trait>`**: only flag when caller count > 1
  (`crabcc callers <fn> --count`). Single-callsite trait objects often
  can't be generic-ified without API churn.
- **Tip 12 — parallelize**: only flag loops where the iteration body has
  no shared `&mut` and the collection length is plausibly large (Agent D
  judges; pure regex would over-fire).
- **Tip 3 — struct layout**: only flag `#[repr(C)]` structs. `#[repr(Rust)]`
  reordering is the compiler's job.
- **Tips 0/2/3/8** are inherently judgment calls — report as "consider",
  never as "fix".

### Token discipline (every sub-agent)

- **Always** use `--count` first to decide if a deeper query is worth running.
- **Never** use `crabcc refs` without `--files-only` or `--limit`.
- **Never** read whole files with `Read`; use `crabcc outline <file>`.
- **Never** call `rg` for code shape — that's what the index is for.
- Skip the agent's body entirely (return `{"findings":[],"skipped_because":"…"}`)
  when the first probe returns 0. Don't pad.

### Agent A — dynamic dispatch & inlining (tips 5, 6, 11)

```bash
# Probe first; cheap.
$CRABCC_BIN fuzzy dyn --limit 30
$CRABCC_BIN fuzzy "inline(always)" --limit 30
# For each Box<dyn T> hit's surrounding fn:
$CRABCC_BIN callers <fn> --count    # skip if count == 1 (FP policy)
# For each #[inline(always)] hit:
$CRABCC_BIN outline <file>          # skip if body ≤ 30 lines
```

### Agent B — data layout & containers (tips 3, 4, 7)

```bash
$CRABCC_BIN files --ext rs --limit 200          # candidate set
$CRABCC_BIN fuzzy "Vec::new" --limit 50
$CRABCC_BIN fuzzy "Vec::with_capacity" --limit 50
# For each candidate hot file:
$CRABCC_BIN outline <file>                       # struct sizes / field order
```

Cross-reference Phase 1's Cargo.toml dep list — only suggest `smallvec` when
not already a dep. Only flag `#[repr(C)]` structs for layout (Rust repr is
the compiler's job).

### Agent C — build profile & dep deltas (tips 0, 1, 10, 12, 13)

Confirms / extends Phase 1, plus:

```bash
$CRABCC_BIN files --under benches --ext rs --limit 5    # tip 0
$CRABCC_BIN fuzzy lazy_static --limit 30                # tip 1
$CRABCC_BIN fuzzy OnceLock --limit 30                   # tip 1
```

If `benches/` is empty, return a **loud** finding: "no benches; later
findings cannot be empirically validated". Cheapest agent — finishes first.

### Agent D — algorithmic & hot-loop review (tips 2, 8, 9)

The only agent allowed to use `repomix`. Steps:

```bash
# 1. Top-3 hot crates by pub-fn count.
for c in "$REPO_PATH"/crates/*; do
  n=$($CRABCC_BIN files --under "$c" --ext rs --limit 1000 | jq '.files | length')
  printf "%s\t%d\n" "$c" "$n"
done | sort -k2 -rn | head -3 | awk '{print $1}' > /tmp/warp-D-hot.txt

# 2. Pack each — never the whole repo.
while read crate; do
  out=/tmp/warp-D-$(basename "$crate").xml
  $REPOMIX_BIN --include "$crate/src/**/*.rs" -o "$out"
done < /tmp/warp-D-hot.txt

# 3. Hunt: nested for over Vec (O(n²)); for i in 0..N small N missing chunks_exact;
#    unsafe get_unchecked near non-asserted indexing.
```

Pack size budget: ≤ 50 KB per crate. If a single crate packs > 50 KB, fall
back to listing its top-10 fns by line count via `crabcc outline` instead.

---

## Phase 3 — Aggregate

Collect the four JSON blocks. Write `<repo>/.crabcc/warp-speed-report.md`:

```markdown
# Warp-speed audit — <repo>

Generated: <ISO> · gist: https://gist.github.com/jFransham/369a86eff00e5f280ed25121454acec1
Tracking:  https://github.com/peterlodri-sec/crabcc/issues/84

## Highest-impact wins
- (Phase 1 Cargo.toml findings)

## Findings by tip
### Tip 6 — Avoid Box<dyn Trait>
- crates/foo/src/bar.rs:42 — `fn do_x(p: Box<dyn Parser>)` — 7 callers — switch to generic
…

## Skipped tips
- Tip 0: no benches under `benches/`; cannot proceed.
```

Print **only** the file path + top 3 findings to chat. The report lives on disk.

---

## Hard rules

1. **Read-only.** Never edit Rust source.
2. **Skip non-Rust repos.** No `Cargo.toml` → exit cleanly.
3. **No whole-repo repomix packs.** Only Agent D packs, and only top-3 hot crates.
4. **Cap at 4 sub-agents.** Adding more linearly costs tokens for sub-linear coverage.
5. **No perf claims without benches.** Surface "no benches" loudly when `benches/` is empty.
6. **Honour the FP policy** in this file's "False-positive policy" section.
7. **Each sub-agent must echo `CRABCC_BIN` / `REPOMIX_BIN` / `REPO_PATH`** at the
   top of its report so a stale-PATH bug is detectable post-hoc.

---

## Cross-references

- [`skill/crabcc/SKILL.md`](../crabcc/SKILL.md) — every `crabcc` invocation in
  this skill follows that skill's tool-ladder. Token-shaping flags
  (`--count`, `--files-only`, `--limit`) come from there.
- [Issue #84](https://github.com/peterlodri-sec/crabcc/issues/84) — tracking
  issue. The full background research (gist→signal mapping, pipeline
  rationale, FP rationale) lives in
  [the first comment](https://github.com/peterlodri-sec/crabcc/issues/84#issuecomment-4350276107).
- [jFransham gist](https://gist.github.com/jFransham/369a86eff00e5f280ed25121454acec1)
  — source material (the 14 tips this skill detects).

---

## Extending

- v2: feed flagged sites into `cargo-show-asm` to confirm bounds-check or
  auto-vectorization status.
- v2: `--apply lto` mode that edits the workspace `Cargo.toml` — the one safe
  automatic fix.
- v2: replace Agent D's repomix step with a streaming `crabcc graph` walk
  once cycle detection is exposed for non-fn nodes.
