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
FLAVOR=c3-32 ./scripts/bench-opt-bin/provision-ovh.sh
# --keep to leave the box up, --quick for the 2-leg smoke
```

Sizing: each LTO leg's link phase is largely single-threaded, so concurrency
≈ `cores / per-leg-jobs`. A **c3-32 (32 vCPU)** runs ~6 legs wide and clears
the fractional matrix (incl. PGO) inside the hour; c3-16 also fits but tighter.

## Promoting a winner

The report prints the exact `RUSTFLAGS` + `--features` for the fastest leg.
PGO/BOLT winners stay opt-in: wire the flags into the `release-nightly` CI
lane (per-target, in the workflow matrix — **not** inlined into the profile,
since `target-cpu`/PGO paths are target-specific), exactly as the profile's
own doc comment instructs.
