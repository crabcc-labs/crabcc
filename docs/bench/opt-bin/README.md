# bench-opt-bin — archived runs

This directory is the **durable, tracked home** for `bench-opt-bin` sweep
output. The harness writes its working artifacts into `bench/` (gitignored,
ephemeral); `scripts/bench-opt-bin/provision-ovh.sh` mirrors the curated
subset of each run here and commits it, so a sweep survives the teardown of
the throwaway OVH box **and** the ephemeral CI container.

Each run lands in `run-<host>-<UTCstamp>/`:

| File | What |
|---|---|
| `REPORT.md` | rendered speed + footprint tables, fastest-config callout |
| `opt-bin.ndjson` | one machine-readable row per leg (all metrics) |
| `MANIFEST.json` | run metadata + per-leg records + sha256 of every artifact |
| `flamegraphs/*.svg` | symbolized flamegraphs for the baseline + fastest legs |
| `logs.tar.gz` | per-leg `cargo build` logs + hyperfine JSONs |

The full, uncurated bundle (including raw per-leg logs) is also written as
`bench/results/run-<host>-<stamp>.tar.gz` on the machine that ran the sweep —
attach that to a GitHub Release or object store if you want the heavyweight
copy preserved too. The per-leg `target/` dirs (tens of GB) are never kept.

See [`scripts/bench-opt-bin/README.md`](../../../scripts/bench-opt-bin/README.md)
for how to run a sweep.
