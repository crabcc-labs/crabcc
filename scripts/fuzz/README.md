# crabcc fuzzing matrix (OVHcloud)

Heavy, unattended libFuzzer run of every `crabcc-core` fuzz target on a fresh
cloud VM. Uses libFuzzer **fork mode** so each target self-parallelizes across
cores and keeps fuzzing after each crash (collecting many distinct crashes per
run, not just the first).

## Recommended OVHcloud instance

CPU-bound work; ASan workers cost ~0.3–0.5 GB RAM each, and we run ~`nproc`
workers, so target **≥2 GB RAM per vCPU**. OVH Public Cloud "Compute" (`c3`)
flavors are the 1:2 vCPU:GB fit:

| run intensity | flavor | vCPU / RAM | notes |
|---|---|---|---|
| sweet spot for 40–50 min | **`c3-16`** | 16 / 32 GB | recommended — strong throughput, modest hourly cost |
| crank it | `c3-32` | 32 / 64 GB | ~2x execs; finishes deeper coverage in the same wall time |
| minimum | `c3-8` | 8 / 16 GB | fine, just fewer workers |

Image: **Ubuntu 24.04** (or 22.04). A small boot disk is enough — the build +
corpus stay well under 10 GB. Confirm exact flavor names against the current
OVH Public Cloud catalog in your region.

## Run it (~45 min)

```bash
# 1. get the source onto the box (private repo: rsync is simplest)
rsync -az --exclude target --exclude .git ./ ubuntu@<ip>:~/crabcc/

# 2. provision toolchain + launch the matrix (DURATION is seconds PER target)
ssh ubuntu@<ip> 'CRABCC_DIR=~/crabcc DURATION=2700 ~/crabcc/scripts/fuzz/provision-ovh.sh'

# 3. pull results back
scp ubuntu@<ip>:'~/crabcc/crates/crabcc-core/fuzz/matrix-*/bundle.tar.gz' .
```

`DURATION=2700` = 45 min per target, run concurrently → ~45 min wall. All six
targets share the box at once (`fork = nproc / 6`).

## Output

Written under `crates/crabcc-core/fuzz/matrix-<UTC-stamp>/`:
- `REPORT.md` — per-target unique-crash count, coverage, exec totals + repro commands
- `logs/<target>.log` — full libFuzzer output per target
- `bundle.tar.gz` — all `artifacts/` (crash reproducers) + grown `corpus/`

Reproduce any crash locally:
```bash
cd crates/crabcc-core
fuzz/target/<triple>/release/<target> <artifact-path>
```

## Known crashes (seeded from the first laptop run)

- `fsst_decompress_arbitrary` — `Codec::decompress` OOB in `fsst-rs` on
  malformed/foreign bytes (UB in release without debug-assertions).
- `md_parse_sanitize` — `md::parse` panics inside `markdown` v1.0.0 instead of
  the documented graceful fallback.

The matrix re-finds these and will surface additional distinct inputs (fork
mode + `-ignore_crashes=1`).
