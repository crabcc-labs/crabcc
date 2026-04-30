---
name: rust-logging-audit
description: Audit a Rust repo for logging / tracing / hot-path discipline. Detects println/eprintln in libraries, log::* vs tracing inconsistency, formatting in hot paths, Mutex<Regex>, blocking sinks, missing OTLP exporter, and lazy first-call initialization. Triggers on "/rust-logging-audit <path>", "rust logging audit", "tracing adoption check", "hot path log audit", or any reference to issue #90, the matthieum low-latency logging post, the Arko log-parsing posts, or the tokio-rs/tracing crate. Uses crabcc + repomix + parallel sub-agents. Read-only; emits `<repo>/.crabcc/rust-logging-report.md`.
---

# rust-logging-audit — apply the matthieum + Arko + tracing playbook to a Rust repo

> Tracking issue: **https://github.com/peterlodri-sec/crabcc/issues/90**
> Migration RFC: [`MIGRATION-RFC.md`](./MIGRATION-RFC.md) (co-located; the
> first comment on issue #90 mirrors it).
> Companion skills:
> - [`skill/crabcc/SKILL.md`](../crabcc/SKILL.md) — symbol-index tool ladder.
> - [`skill/warp-speed-audit/SKILL.md`](../warp-speed-audit/SKILL.md) — same
>   fan-out architecture; this skill is its sibling for logging.

Source theses ([issue #90](https://github.com/peterlodri-sec/crabcc/issues/90)):

- **[Arko 2025](https://andre.arko.net/2025/03/28/rust-keeps-parsing-those-logs-faster/)** — `Mutex<Regex>` is the bottleneck; remove it. zlib-ng beats C zlib on x86.
- **[Ochagavía / matthieum 2023](https://ochagavia.nl/blog/low-latency-logging-in-rust/)** — partition work into compile-time / init-time / hot-path. No allocs in the hot path. No lazy first-call init.
- **[tokio-rs/tracing](https://github.com/tokio-rs/tracing)** — the framework. Pairs with `tracing-opentelemetry` for OTLP export.

This file is **self-contained** — sub-agents need only the prompt their
orchestrator writes from this skill plus a working `crabcc` binary.

**Inputs**:  a Rust repo path (defaults to `pwd`).
**Output**:  `<repo>/.crabcc/rust-logging-report.md`.
**Side effects**:  `<repo>/.crabcc/index.db` if missing.
**Read-only** to source code.

---

## When this skill fires

- `/rust-logging-audit [path]`.
- The user references issue #90, the matthieum post, the Arko post, or
  `tokio-rs/tracing` and asks to audit / migrate / score a Rust codebase.
- The user pastes any of the three source URLs.

If `<path>` has no `Cargo.toml`, exit cleanly. Don't run on non-Rust repos.

---

## Phase 0 — ENV/PATH bootstrap (run ONCE, share with every sub-agent)

Same pattern as `warp-speed-audit`: resolve binaries up front, pass them as
environment variables in every sub-agent prompt. Sub-agents inherit the
parent's env but **not** the interactive shell's rc-files.

```bash
export REPO_PATH="${1:-$PWD}"
export CRABCC_BIN="${CRABCC_BIN:-$(command -v crabcc || echo "$HOME/.cargo/bin/crabcc")}"
export REPOMIX_BIN="${REPOMIX_BIN:-$(command -v repomix || echo "npx --yes repomix")}"

test -x "$CRABCC_BIN" || { echo "crabcc not found — install via cargo install crabcc"; exit 1; }
case ":$PATH:" in *":$HOME/.cargo/bin:"*) ;; *) export PATH="$HOME/.cargo/bin:$PATH" ;; esac

cd "$REPO_PATH"
test -f .crabcc/index.db || "$CRABCC_BIN" index
"$CRABCC_BIN" refresh
```

Every sub-agent prompt MUST start with the four resolved values
(`REPO_PATH`, `CRABCC_BIN`, `REPOMIX_BIN`, augmented `PATH`) and MUST echo
them at the top of its report.

---

## Phase 1 — Cargo.toml fast-path (synchronous, before fan-out)

Read `<repo>/Cargo.toml` and `crates/*/Cargo.toml`. These findings are the
"highest-impact wins" and don't deserve an agent.

| Tip                              | Check                                                              |
|----------------------------------|--------------------------------------------------------------------|
| **tracing adoption**             | Workspace deps include `tracing` AND `tracing-subscriber`?         |
| **non-blocking writer**          | Workspace deps include `tracing-appender`?                         |
| **OTLP exporter**                | Workspace deps include `tracing-opentelemetry` + `opentelemetry-otlp`? |
| **`log` ↔ `tracing` bridge**     | If both `log` and `tracing` are present, is `tracing-log` wired?   |
| **mixed framework smell**        | Any of `slog`, `fern`, `env_logger` AND `tracing-subscriber`?       |
| **zlib-ng**                      | If `flate2` present, is `zlib-ng` feature on (Arko's x86 win)?     |
| **regex shape**                  | If `regex` present, flag for hot-path mutex check (see Agent A).   |

The fan-out agents will not re-check these.

---

## Phase 2 — Parallel fan-out (4 sub-agents in ONE message)

Send a single message with **four `Agent` tool calls** (`general-purpose`).
Each prompt is self-contained: source links, owned cluster, exact
`$CRABCC_BIN` / `$REPOMIX_BIN` commands, the JSON contract below, the FP
policy below. Cap each report ≤ 300 words.

### JSON output contract

```json
{
  "agent": "A | B | C | D",
  "cluster": "library-noise | hot-path | init-time | framework-mix",
  "env_echo": { "REPO_PATH": "...", "CRABCC_BIN": "...", "REPOMIX_BIN": "..." },
  "findings": [
    {
      "rule": "no-eprintln-in-lib | format-on-hot-path | mutex-regex | …",
      "file": "crates/foo/src/bar.rs",
      "line": 42,
      "severity": "low | medium | high",
      "snippet": "eprintln!(\"...\")",
      "suggestion": "Replace with `tracing::warn!(…)` and pass field values."
    }
  ],
  "skipped_because": null
}
```

If the first probe returns 0 hits, return `{"findings": [],
"skipped_because": "<reason>"}`. Don't pad.

### False-positive policy

- **`println!` in `crates/*-cli/src/main.rs` or under `examples/`** is
  legitimate user-facing output. Skip.
- **`eprintln!` inside `#[cfg(test)]` or `tests/`** is fine. Skip.
- **`#[cfg(debug_assertions)] eprintln!`** is fine. Skip.
- **`Mutex<Regex>`** flagged ONLY when called from a fn that is itself called
  from a loop or from a `tokio::spawn` task. Use
  `crabcc graph walk <fn> --dir callers --depth 3` to confirm.
- **`format!` macros** flagged only inside the body of a `tracing::*` /
  `log::*` macro call site (i.e. format-then-log antipattern).
- **`info!("text")` with no fields** is fine — the cost is in `format_args!`,
  not the log call.

### Token discipline

- Always `--count` first; skip body if zero.
- Always cap with `--limit 30` / `--files-only`.
- Never `Read` whole files; use `crabcc outline <file>`.
- Never `rg` for code shape — use the index.

### Agent A — library noise (println, eprintln, log::* in libs)

```bash
$CRABCC_BIN fuzzy "println!" --limit 50
$CRABCC_BIN fuzzy "eprintln!" --limit 50
$CRABCC_BIN fuzzy "log::info" --limit 30
$CRABCC_BIN fuzzy "log::warn" --limit 30
$CRABCC_BIN fuzzy "log::error" --limit 30
$CRABCC_BIN fuzzy "log::debug" --limit 30
```

For each hit, derive the crate from the file path. Skip if crate ends in
`-cli` and the file is `main.rs` or `bin/*.rs`. Skip if the file is under
`examples/` or `tests/` or has `#[cfg(test)]` near the line (use
`crabcc outline` line range to check).

### Agent B — hot-path discipline (matthieum)

```bash
$CRABCC_BIN fuzzy "Mutex<Regex" --limit 20
$CRABCC_BIN fuzzy "Mutex::new" --limit 30        # then filter for Regex / Vec<u8> formatting buffers
$CRABCC_BIN fuzzy "format!" --limit 50            # then locate inside tracing!/log! call sites
$CRABCC_BIN fuzzy "to_string()" --limit 30        # eager allocs in span fields
```

For each `Mutex<Regex>` hit: `crabcc graph walk <containing_fn>
--dir callers --depth 3`. Flag only when a caller is itself in a loop or a
spawn closure (use `crabcc outline <file>` and look for `for`, `while`,
`tokio::spawn`, `rayon::scope`).

### Agent C — init-time hygiene (matthieum)

```bash
$CRABCC_BIN fuzzy "lazy_static!" --limit 30
$CRABCC_BIN fuzzy "OnceLock" --limit 30
$CRABCC_BIN fuzzy "OnceCell" --limit 30
$CRABCC_BIN fuzzy "tracing_subscriber" --limit 20
$CRABCC_BIN fuzzy "init_telemetry" --limit 20
```

Flag any `lazy_static!` / `OnceLock` whose initializer can plausibly take
> 1 ms (regex compilation, file I/O, network call) AND that is used in code
reachable from a `tracing::*` macro. matthieum's rule: lazy first-call is
the worst kind of jitter. Recommend init-time evaluation in `main()`.

Confirm a `tracing_subscriber::*::init()` call exists in each binary crate's
`main.rs`. Flag binary crates that have none.

### Agent D — framework mix & OTLP wiring

The only agent allowed to use `repomix`. Steps:

1. Identify the binary crates: `$CRABCC_BIN files --ext rs --limit 200`
   then filter for `*/main.rs`.
2. For each binary crate, pack only its `src/` (≤ 50 KB budget) with
   `$REPOMIX_BIN --include "<crate>/src/**/*.rs" -o /tmp/log-D-<name>.xml`.
3. In the packed output, look for:
   - `slog`, `fern`, `env_logger` mixed with `tracing_subscriber`.
   - Multiple competing global subscribers / `init()` calls.
   - Missing `tracing-opentelemetry` wiring when `Cargo.toml` declared it.
   - Missing `tracing_appender::non_blocking` when log volume is plausibly
     high (heuristic: any `tracing::trace!` or `tracing::debug!` inside a
     loop body).

Pack budget: ≤ 50 KB per crate. Over-budget → fall back to
`crabcc outline <main.rs>` and the first 200 lines via Read.

---

## Phase 3 — Aggregate

Write `<repo>/.crabcc/rust-logging-report.md`:

```markdown
# Rust logging audit — <repo>

Generated: <ISO> · issue #90 · [tracing](https://github.com/tokio-rs/tracing)

## Highest-impact wins
- (Phase 1 Cargo.toml findings)

## Findings by cluster
### Library noise (Agent A)
- crates/foo/src/bar.rs:42 — `eprintln!(…)` in library code → `tracing::warn!`

### Hot-path discipline (Agent B)
- crates/baz/src/parse.rs:88 — `Mutex<Regex>` reachable from a parallel loop → per-thread Regex

### Init-time hygiene (Agent C)
- crates/qux/src/lib.rs:14 — `lazy_static!` with regex compile reachable from `info!` site → eager init in main

### Framework mix & OTLP (Agent D)
- crates/cli/src/main.rs:9 — `env_logger::init()` AND `tracing_subscriber::fmt::init()` — pick one

## Skipped clusters
…

## Score
<aggregate severity → "good" | "needs work" | "critical">
```

Print **only** the file path + top 3 findings to chat.

---

## Hard rules

1. **Read-only.** Never edit Rust source.
2. **Skip non-Rust repos.** No `Cargo.toml` → exit cleanly.
3. **No whole-repo repomix packs.** Only Agent D, only main.rs of binary crates, ≤ 50 KB.
4. **Cap at 4 sub-agents.**
5. **Honour the FP policy** — `println!` in `*-cli/main.rs`, examples, tests, and `cfg(debug_assertions)` are not findings.
6. **Each sub-agent must echo** `CRABCC_BIN` / `REPOMIX_BIN` / `REPO_PATH` in its report header.
7. **Don't recommend** without checking `Cargo.toml` first — many findings are already-fixed if a dep is present.

---

## Cross-references

- [`MIGRATION-RFC.md`](./MIGRATION-RFC.md) — concrete 4-phase migration plan
  for crabcc itself, drafted as the C deliverable on issue #90. Useful as a
  template for the user's own project.
- [`skill/warp-speed-audit/SKILL.md`](../warp-speed-audit/SKILL.md) — sister
  skill; same fan-out architecture and ENV bootstrap.
- [`skill/crabcc/SKILL.md`](../crabcc/SKILL.md) — every `crabcc` command in
  this skill follows that file's tool ladder and token-shaping flags.
- [Issue #90](https://github.com/peterlodri-sec/crabcc/issues/90) — research
  and adoption plan.
- [Issue #86](https://github.com/peterlodri-sec/crabcc/issues/86) — rotel
  `/live` panel, the OTLP terminus this skill expects to find configured.

## Extending

- v2: emit a machine-readable `report.json` alongside the markdown so other
  tools can consume findings.
- v2: optional `--apply tracing-init` mode that wires
  `tracing_subscriber::fmt::init()` + `tracing_appender::non_blocking` into
  `main.rs` — the one safe automatic fix.
- v2: integrate with `cargo-show-asm` to confirm hot-path log calls compile
  to a single `cmp + jmp` (matthieum's atomic-ID activation gate).
