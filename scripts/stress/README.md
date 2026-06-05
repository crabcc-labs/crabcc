# crabcc stress + fuzz harness

`stress.py` launches many workers in parallel and hammers the real `crabcc`
subcommand surface, mixing **valid** args (symbol names / qualified names /
files harvested live from the index DB) with **fuzzed** args (empty, huge,
unicode, format-string, path-traversal, control chars, regex bombs).

The goal is to surface *real* defects — panics, segfaults, deadlocks, hangs —
as distinct from the clean non-zero exits bad input is *supposed* to produce.

## Run

```bash
task stress                       # 16 readers + 2 writers, 30s
task stress WORKERS=32 WRITERS=4 DURATION=60 SEED=7
task stress-smoke                 # fast deterministic read-only smoke (CI)

# or directly:
python3 scripts/stress/stress.py --workers 16 --writers 3 --duration 30
python3 scripts/stress/stress.py --iterations 2000 --fuzz-rate 0.5 --seed 1
```

Stdlib only — no pip installs. Auto-detects `target/release/crabcc` →
`target/debug/crabcc` → `$PATH` (override with `--bin`), and the newest index
DB under `$CRABCC_HOME/repos/*/index.db` (override with `--db`).

## What it does

- **Readers** run `lookup {sym,refs,callers,outline,fuzzy,prefix,grep,files}`,
  `graph {walk,cycles,orphans}`, `memory {search,list,count}`, `info`, `read`.
- **Writers** (`--writers N`) run `index refresh`, `graph build`,
  `memory remember` concurrently against the shared SQLite/WAL DB — this is the
  path most likely to expose locking/contention/corruption bugs.
- Each invocation is classified:
  | outcome | meaning |
  |---|---|
  | `OK` | exit 0 |
  | `CLEAN_ERR` | non-zero exit with a clean error (expected for bad input) |
  | `CRASH` | signal (segv/abrt), Rust panic (exit 101), or `panicked at` in stderr — **a bug** |
  | `TIMEOUT` | exceeded `--cmd-timeout` — a hang, **a bug** |
  | `UNRUNNABLE` | arg can't reach `execve` (embedded NUL, E2BIG) — harness limit, not a defect |

## Output

Writes to `bench/stress/` (gitignored):

- `stress.ndjson` — one JSON record per invocation (argv, rc, ms, outcome, stderr head).
- `stress-REPORT.md` — outcome totals, **verdict**, a crashes-with-repro section,
  per-subcommand latency (p50/p95/p99/max), and the top clean-error signatures.

The process **exits non-zero if any CRASH or TIMEOUT is seen**, so `task stress`
can gate CI. Use `--seed` for reproducible runs (fuzz args are deterministic per
seed).

## Knobs

| flag | default | meaning |
|---|---|---|
| `--workers` | `min(cpu,16)` | reader worker count |
| `--writers` | `0` | concurrent DB-mutating workers |
| `--duration` / `--iterations` | `30s` | run length (mutually exclusive) |
| `--fuzz-rate` | `0.35` | fraction of args that get mutated |
| `--cmd-timeout` | `30s` | per-invocation timeout (→ `TIMEOUT`) |
| `--seed` | random | RNG seed for reproducibility |
| `--bin` / `--db` / `--out` | auto | overrides |
