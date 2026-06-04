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

Or via Task: `task bench-opt-bin` (smoke) / `task bench-opt-bin-full`.

Outputs land in `bench/results/` (gitignored, matching every other bench):
- `opt-bin.ndjson` — one row per leg (all metrics, machine-readable)
- `opt-bin-REPORT.md` — speed table (Δ% vs baseline), footprint table, and a
  paste-ready `RUSTFLAGS` block for the fastest config.

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
