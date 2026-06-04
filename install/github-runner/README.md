# GitHub Actions — Hetzner self-hosted runners

Linux CI, Linear sync, and release builds should run on the **Hetzner**
pool, not `ubuntu-latest` (avoids GitHub-hosted billing limits and fixes
private-repo checkout for light workflows).

## Runner labels

Workflows use:

```yaml
runs-on: [self-hosted, linux, hetzner]
```

Register the runner with at least: `self-hosted`, `linux`, `hetzner`.

Optional fourth label `crabcc` if you run multiple repos on one host.

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
gives `TMPDIR`, `CARGO_HOME`, and `SCCACHE_DIR` their own partition (20–50 GB).

### 1 — Add a volume in the Hetzner console

Cloud Console → server → **Volumes** → Create Volume → 40 GB, same DC.
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
RUNNER_TOOL_CACHE=/var/runner-data/tool-cache
```

All child processes (including `dtolnay/rust-toolchain` and cargo) inherit
`TMPDIR` and write to the data volume instead of the root filesystem.

## Verify

```bash
systemctl status actions-runner
# GitHub UI → Settings → Actions → Runners → should show Idle (hetzner)
```

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
