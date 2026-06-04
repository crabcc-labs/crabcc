# bench-opt-bin — where sweep output goes

Sweep artifacts are **not** committed into this repo (`bench/` is gitignored,
and we don't want tarballs/flamegraphs bloating `crabcc`'s history). Each run
is bundled locally into `bench/results/run-<host>-<stamp>/` (+ a `.tar.gz` and
a `MANIFEST.json` with a sha256 of every file), then
[`scripts/bench-opt-bin/publish.sh`](../../../scripts/bench-opt-bin/publish.sh)
fans it out to whichever durable sinks are configured:

| Sink | Env to enable | What lands there |
|---|---|---|
| **`_bench-results` repo** (system of record) | `BENCH_RESULTS_REPO` | full run under `runs/<id>/`; `*.tar.gz` + `*.svg` via **Git LFS**, reports/NDJSON/manifest as plain diffable text |
| **Discord** notification | `COMPOSIO_API_KEY`, `COMPOSIO_DISCORD_ACCOUNT`, `DISCORD_CHANNEL_ID` | REPORT summary + fastest-config line, via Composio |
| **Google Drive** | `COMPOSIO_API_KEY`, `COMPOSIO_DRIVE_ACCOUNT` | the zipped bundle, via Composio |

`provision-ovh.sh` calls `publish.sh` automatically after pulling results back
(`PUBLISH=0` to skip). Because `publish.sh` runs on *your* machine, it uses
your git credentials and `COMPOSIO_API_KEY` / `~/.composio` — not the
throwaway VM's.

This directory itself only holds documentation, not run artifacts. See
[`scripts/bench-opt-bin/README.md`](../../../scripts/bench-opt-bin/README.md)
for how to run a sweep.
