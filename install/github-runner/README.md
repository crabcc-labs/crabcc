# GitHub Actions — Hetzner self-hosted runners

Linux CI, Linear sync, and release builds should run on the **Hetzner**
pool, not `ubuntu-latest` (avoids GitHub-hosted billing limits and fixes
private-repo checkout for light workflows).

## Runner labels

Build CI workflows (`fmt`, `lint`, `test`) require:

```yaml
runs-on: [self-hosted, linux, hetzner, gh-runner]
```

The installer now defaults to all four labels. If you register a runner
manually or re-register an existing one, ensure it carries `gh-runner` —
without it the runner will never pick up build jobs (they queue indefinitely).

The `gh-runner` label deliberately excludes specialised runners (e.g. NixOS
bench-node) from build CI. Bench-node and similar hosts should be registered
**without** `gh-runner` so they only pick up workflows that explicitly target
their labels.

## One-time install (Hetzner box)

```bash
# As root or deploy user with passwordless sudo
sudo bash install/github-runner/install.sh \
  --url https://github.com/peterlodri-sec/crabcc \
  --token <REGISTRATION_TOKEN>
```

Registration token: **GitHub → repo → Settings → Actions → Runners → New self-hosted runner**.

The script installs OS packages (Rust build deps, mold, python3), downloads
the latest `actions-runner` release, registers, and installs a **systemd**
unit `actions-runner.service`.

## Dedicated cache volume (recommended for runner-01 / runner-01b)

The recurring `curl: (23) Failure writing output` failure is caused by the
root filesystem filling up while `dtolnay/rust-toolchain` downloads a Rust
toolchain tarball to `/tmp`.  The fix is a dedicated Hetzner volume that
gives `TMPDIR`, `CARGO_HOME`, and `SCCACHE_DIR` their own partition.

**Sizing: provision a 100 GB volume on _each_ runner host (runner-01 and
runner-01b).** Earlier guidance was 40 GB; in practice cargo + sccache +
the tool-cache outgrow that and jobs start hitting `No space left on
device`, so 100 GB is the new standard for both runners.

> **Root-fs caveat.** This volume only relieves the paths it backs
> (`/var/runner-data/{tmp,cargo,sccache,tool-cache}`). `apt-get` still
> writes to `/var/cache/apt` and `/var/lib/apt` on the **root** filesystem,
> so a job step like `apt-get install mold ripgrep` can still fail with
> `E: Write error - write (28: No space left on device)` even with a large
> cache volume. To fully prevent that, either (a) grow the server's root
> disk (Hetzner server *resize*, which expands `/`), or (b) preinstall
> `mold` + `ripgrep` into the runner image so no per-job `apt` write is
> needed, or (c) point apt's cache at the data volume. Growing the root
> disk is the simplest and is recommended alongside the 100 GB volume.

### 1 — Add a volume in the Hetzner console

Cloud Console → server → **Volumes** → Create Volume → **100 GB**, same DC.
Attach to `runner-01` (and separately to `runner-01b`).
The volume appears as `/dev/disk/by-id/scsi-0HC_Volume_<id>` and `/dev/sdb`
(or `/dev/sdc` if a second volume is already attached).

Confirm the device name on the box:
```bash
lsblk -o NAME,SIZE,MOUNTPOINT,LABEL
```

### 2 — Provision the volume (no re-registration required)

```bash
# On runner-01
sudo bash install/github-runner/install.sh \
  --gc-only --cache-volume /dev/sdb

# On runner-01b
sudo bash install/github-runner/install.sh \
  --gc-only --cache-volume /dev/sdb
```

`--gc-only --cache-volume` formats the device as ext4 (skipped if already
ext4), adds a `/etc/fstab` entry (`nofail`), mounts it at `/var/runner-data`,
and refreshes the GC timer.

**The runner service is not restarted by `--gc-only`.**  After the volume
is mounted, restart it once to pick up the new env vars:

```bash
sudo systemctl restart actions-runner
```

### 3 — Full fresh install with a volume

```bash
sudo bash install/github-runner/install.sh \
  --url https://github.com/peterlodri-sec/crabcc \
  --token <REGISTRATION_TOKEN> \
  --cache-volume /dev/sdb
```

The install script sets these env vars in the systemd unit automatically:

```
TMPDIR=/var/runner-data/tmp
CARGO_HOME=/var/runner-data/cargo
SCCACHE_DIR=/var/runner-data/sccache
SCCACHE_CACHE_SIZE=15G
RUNNER_TOOL_CACHE=/var/runner-data/tool-cache
CARGO_TARGET_DIR=/var/runner-data/target/<runner-name>
```

All child processes (including `dtolnay/rust-toolchain` and cargo) inherit
these and write to the data volume instead of the root filesystem.

**`CARGO_TARGET_DIR` is the important one for disk pressure.** Without it,
every build writes a multi-GB `target/` tree into the job checkout under
`_work` on the **root fs** — the volume would mount but barely get used, and
the root fs fills until a later `apt-get` / toolchain download dies with
`No space left on device`. It is namespaced per runner
(`/var/runner-data/target/<runner-name>`) so two runner processes on one host
don't collide on cargo's exclusive target-dir lock. The GC timer prunes
per-runner target dirs untouched for 7 days.

> **sccache backend.** In CI the workflow sets `SCCACHE_GHA_ENABLED=true`, so
> sccache uses the GitHub Actions cache service (shared across runners), not
> the local `SCCACHE_DIR` on the volume. `SCCACHE_DIR` is the fallback for
> non-GHA / local runs. Both are wired; the GHA backend is what you see in the
> per-job `sccache --show-stats` (`Cache location: ghac`).

## Host tuning (preinstalled tools, fd limits, shm)

`install.sh` also applies these host-level settings (and `--gc-only` re-applies
them idempotently, so an existing fleet picks them up on the next run):

- **Preinstalled CI tools.** The apt step installs everything the workflows
  install at job time — `mold`, `ripgrep`, `sqlite3`, `zstd`, … — so the
  per-job `command -v X || apt-get install X` guards are all no-ops. No per-job
  `apt` writes to the root fs ⇒ no `apt`-time `No space left on device`. **Keep
  this list in sync with `.github/workflows/*` whenever a job adds a tool.**
- **File-descriptor / process limits.** The runner unit sets
  `LimitNOFILE=1048576` and `LimitNPROC=unlimited` — the systemd default of
  1024 open files is far too low for big parallel `rustc`/link jobs + sccache.
- **`/dev/shm`.** Host shm is pinned to 6 GB (`SHM_SIZE=` overrides), and the
  Docker daemon's `default-shm-size` is bumped from 64 MB to 2 GB for the
  testcontainers e2e suite.

## Verify

After provisioning, confirm the runner actually uses the volume:

```bash
systemctl status actions-runner
# GitHub UI → Settings → Actions → Runners → should show Idle (hetzner)

# Env vars present in the unit (TMPDIR, CARGO_HOME, CARGO_TARGET_DIR, …):
systemctl show -p Environment actions-runner   # or actions.runner.*.service

# Volume mounted, and target/cargo actually landing on it (not root):
df -h /var/runner-data
du -sh /var/runner-data/* 2>/dev/null
```

A correctly-wired runner shows `/var/runner-data/{cargo,target,tool-cache}`
growing during/after a build while `df -h /` (root) stays flat.

Trigger **Linear sync** or any PR — jobs should queue on the Hetzner runner,
not `ubuntu-latest`.

## Disk GC

`install.sh` also installs **`actions-runner-gc.timer`**, a host-local
systemd timer that runs `runner-gc.sh` ~every 6h to prune Docker
image/build cache, regenerable cargo caches, apt/journald, and stale job
temp. This is what keeps the box from filling up (a job dying with
`No space left on device` is the symptom it prevents).

When a cache volume is configured, `runner-gc.sh` also cleans
`/var/runner-data/tmp` (stale entries older than 1 day) and reports
fill percentages for both the root fs and the data volume.

It's host-local on purpose: every runner shares the labels
`self-hosted, linux, hetzner`, so a single GitHub Actions cron would only
ever clean one host. The timer makes each box clean **itself**.

```bash
systemctl list-timers actions-runner-gc.timer   # next/last run
sudo systemctl start actions-runner-gc.service   # run GC now
journalctl -u actions-runner-gc.service          # see what it pruned
```

> **Rollout to an existing fleet:** run `install.sh --gc-only` on each box
> — it installs just the GC script + timer, **no registration token and no
> runner re-registration**:
>
> ```bash
> for h in runner-01 runner-01b; do
>   ssh root@"$h" 'cd /opt/crabcc && git pull && \
>     sudo bash install/github-runner/install.sh --gc-only'
> done
> ```
>
> `--gc-only` auto-detects the runner's user + working dir from the
> existing `actions-runner.service`, so it targets the right account even
> when the runner runs as a non-root `deploy` user (pass `--user <name>` to
> override).

## Runner health dashboard

Actions → **runner-health** → Run workflow (or wait for the scheduled run
every 2h).  The job summary shows per-runner disk / RAM / CPU metrics and
a fleet overview with status + busy flags pulled from the GitHub API.

## Preinstalled toolchain (recommended)

For fast CI, bake on the runner once:

- `rustup` stable + `rustfmt`, `clippy`
- `cargo-nextest`
- `mold`, `clang`, `git`, `jq`, `python3`, `sqlite3`, `zstd`

The install script installs apt packages; run `rustup` manually on the host
or extend `install.sh` if you want fully hermetic jobs without
`dtolnay/rust-toolchain` on every run.

## macOS

Release/nightly **aarch64-apple-darwin** legs still use `macos-latest`
(GitHub-hosted) until a Mac self-hosted runner exists.
