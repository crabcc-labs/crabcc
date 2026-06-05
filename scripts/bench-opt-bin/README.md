# bench-opt-bin

A binary-optimization sweep that goes **past** the shipped release baseline
(`opt-level=3` / `lto="fat"` / `codegen-units=1` / `panic="abort"` / `strip`)
and measures the high-effort axes that need real telemetry or post-link work:

```
target-cpu × allocator × PGO{off,on} × BOLT{off,on}   (+ opt-level=z corner)
```

Each leg builds `crabcc` on the reserved **`release-nightly`** profile (the
workspace `Cargo.toml` calls out that profile as the home for "PGO, BOLT,
opt-level=z experiments"), drives a realistic heavy workload (`crabcc index`
over a fixture repo) for PGO/BOLT telemetry, then measures **runtime**
(hyperfine on cold `crabcc index` + a warm `sym` query) and **footprint**
(`size -A`, file size, optional `cargo bloat`).

## Why two phases

Builds dominate the wall clock and they fan out across cores — so **Phase A
builds all legs in parallel**, each in its own `CARGO_TARGET_DIR` (differing
`RUSTFLAGS` would otherwise thrash a shared cache). Timing is poisoned by a
busy box, so **Phase B measures serially**, optionally `taskset`-pinned. That
split is what makes a deep matrix fit in ~1 hour without lying about the
numbers.

## Fully utilizing the box

`fat` LTO's final link is **largely single-threaded**, so a handful of wide
builds leaves most cores idle during the link. To actually fill the machine:

1. **One leg per core** — `--jobs` defaults to `min(legs, cores)`, not a small
   pool. Per-leg `CARGO_BUILD_JOBS` is oversubscribed ~1.5× (`--saturate`,
   default on) so the parallel dep-codegen bursts soak up the cores the LTO
   links leave idle.
2. **Enough legs** — with fewer legs than cores, the LTO phase under-fills no
   matter how you schedule it. `--deep` (the 28-leg matrix, adds the
   v2/v3/v4/native target-cpu axis) gives a 32-vCPU box enough independent
   work to stay full. The harness prints a `⚠ legs < cores` warning when you'd
   leave the box half-idle.
3. **sccache** — auto-enabled when on `PATH`. Per-leg target dirs otherwise
   recompile the whole dependency tree N times; sccache shares cached crate
   artifacts across legs with matching flags, converting that redundancy into
   useful throughput. `provision-ovh.sh` installs it and prints `--show-stats`.

Phase B (measurement) is deliberately serial and leaves the box idle — that's
the price of trustworthy timings. Keep it short by pinning to a few cores
(`--pin 0-3`); the build phase is where the rented hour is actually spent.

## Run it locally

```bash
# Smoke (no PGO/BOLT, 2 legs) — proves the harness shape:
python3 scripts/bench-opt-bin/sweep.py --quick

# See the plan without building:
python3 scripts/bench-opt-bin/sweep.py --dry-run

# Full fractional matrix (~11 legs):
python3 scripts/bench-opt-bin/sweep.py --jobs 6 --pin 0-3
```

## Memory guard (small boxes)

`--max-mem-gib` (default: 85% of `MemAvailable`) caps the build pool so
`jobs × --per-leg-mem-gib` (default 4 GiB — the fat-LTO link / BOLT-rewrite
high-water mark) stays under budget, downscaling the pool and warning if a
single leg still wouldn't fit. This is what lets the deep matrix run on a
16 GiB box without OOM-killing the parallel LTO links.

## Scenario × arch — per-task, per-machine configs

PGO and BOLT are *workload-specific by construction*: a profile gathered on
indexing optimizes the index path; one gathered on lookups optimizes queries.
So the harness has a **scenario** axis that shapes both the PGO/BOLT telemetry
(build) and the measured ops (Phase B):

```bash
--scenario index    # parse/write heavy (default) — primary `index`, 2nd `lookup sym`
--scenario lookup   # query heavy            — primary `lookup refs`, 2nd `lookup callers`
--scenario graph    # call-graph traversal   — primary `graph walk`, 2nd `lookup sym`
```

Run the sweep **once per scenario** and each emits its own winner — that's the
"base config per task/agent group" map. The `--arch` axis makes the target-cpu
legs architecture-aware:

```bash
--arch x86-64    # x86-64-v2/v3/v4/native (default on x86 hosts)
--arch aarch64   # neoverse-n1/v1/v2/native — AWS Graviton2/3/4
```

aarch64 legs need an aarch64 toolchain; cross-building from x86 warns and the
legs will error/skip — run them *on* a Graviton box. (crabcc's only vector hot
path, `cosine`, uses `std::simd` and lowers to NEON automatically, so there's no
x86-only SIMD penalty on ARM.)

## Measurement isolation (trustworthy small deltas)

PGO/BOLT wins are often 2–5% — smaller than the run-to-run noise on a busy or
unpinned box. Two flags cut that variance:

```bash
--pin 0-3        # taskset the measured leg to dedicated cores + chrt -b (SCHED_BATCH)
--tmpfs[=GiB]    # mount a tmpfs for the measurement fixture (default 4 GiB),
                 # taking disk/fsync jitter out of the timed op (needs CAP_SYS_ADMIN)
```

Or via Task: `task bench-opt-bin` (smoke) / `task bench-opt-bin-full`.

Outputs land in `bench/results/` (gitignored, matching every other bench):
- `opt-bin.ndjson` — one row per leg (all metrics, machine-readable)
- `opt-bin-REPORT.md` — speed table (Δ% vs baseline), footprint table, and a
  paste-ready `RUSTFLAGS` block for the fastest config.

## Saving / backing up output

Every run is bundled into a self-contained, timestamped
`bench/results/run-<host>-<stamp>/` (REPORT, NDJSON, per-leg build logs,
hyperfine JSONs, flamegraphs) plus a `.tar.gz` and a `MANIFEST.json` carrying
a sha256 of each artifact. Because `bench/` is gitignored — and both the OVH
box and the CI container are ephemeral — pass `--archive-dir` to *also* mirror
the curated subset into a **tracked** path; committing that is what actually
preserves the run:

```bash
python3 scripts/bench-opt-bin/sweep.py --deep --flamegraph \
  --archive-dir docs/bench/opt-bin
```

For unattended/OVH runs, durable backup is handled by
[`publish.sh`](./publish.sh), not `--archive-dir`: `provision-ovh.sh` runs the
sweep, pulls the bundle back, then calls `publish.sh`, which fans the run out
to whichever sinks are configured (the `_bench-results` repo + LFS, a Discord
webhook, Google Drive — see the table below). With **no** sink env set, nothing
is published and the bundle stays under gitignored `bench/results/` only —
so set at least `BENCH_RESULTS_REPO` if you want the run persisted. (`PUBLISH=0`
skips the publish step entirely.) `--archive-dir` above is the manual,
local-run equivalent if you'd rather mirror into a tracked path yourself.

`--flamegraph` renders symbolized SVGs for the baseline + fastest legs by
rebuilding just those two under the `profiling` profile (debug info, no strip —
the same one `task flamegraph-index` uses), so perf can resolve symbols.
Needs `cargo-flamegraph` + `perf`; skipped with a note if absent.

### Requirements

`cargo`, `rustc`, `hyperfine`, `size` (binutils), and `llvm-profdata`
(`rustup component add llvm-tools-preview`). BOLT legs also need `llvm-bolt`
+ `merge-fdata`; missing tools cause those legs to be **skipped**, not fail.

## Run it on a throwaway OVHcloud box

OVH Public Cloud bills per-minute, so a one-hour ad-hoc instance is the right
product (bare-metal Advance/Scale are monthly). `provision-ovh.sh` drives the
OpenStack CLI: it creates the VM, installs the toolchain, rsyncs the repo,
runs the sweep, pulls `bench/results/` back, and **deletes the VM on every
exit path** (a `trap` guards against leaving a billable box running).

```bash
source ~/ovh-openrc.sh        # OpenStack RC file v3 from Horizon
./scripts/bench-opt-bin/provision-ovh.sh         # defaults: c3-64 + --deep
# FLAVOR=c3-32 ... to downsize, --keep to leave the box up, --quick for smoke
```

### Sizing & cost

OVH `c3` suffix is **RAM in GiB** at 1 vCPU : 2 GiB, so `c3-64` = 32 vCPU. The
default 28-leg `--deep` matrix is sized to keep all 32 vCPU busy through the
LTO links. Public-cloud bills per minute and outbound traffic is free (except
APAC), so a sub-hour run costs a fraction of the hourly rate:

| Flavor | vCPU / RAM | ~On-demand /hr | Notes |
|---|---|--:|---|
| `c3-16` | 8 / 16 GiB | ~$0.215 | tight; use the 11-leg fractional matrix |
| `c3-32` | 16 / 32 GiB | ~$0.431 | fractional matrix fits comfortably |
| **`c3-64`** | **32 / 64 GiB** | **~$0.850** | **default — runs `--deep` (28 legs) inside ~1h** |
| `c3-128` | 64 / 128 GiB | ~$1.70 | overkill unless you widen the matrix further |

Rates are OVH on-demand (mid-2026, region-dependent — verify at checkout). A
`c3-64` run that finishes in ~45 min costs **well under \$1** all-in. The
`trap` in `provision-ovh.sh` deletes the VM on every exit path, so the
only real cost risk — a forgotten running box — is guarded.

## Promoting a winner

The report prints the exact `RUSTFLAGS` + `--features` for the fastest leg.
PGO/BOLT winners stay opt-in: wire the flags into the `release-nightly` CI
lane (per-target, in the workflow matrix — **not** inlined into the profile,
since `target-cpu`/PGO paths are target-specific), exactly as the profile's
own doc comment instructs.
