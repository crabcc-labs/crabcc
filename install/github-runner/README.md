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
> for h in hetzner-1 hetzner-2 hetzner-3; do   # your hosts
>   ssh root@"$h" 'cd /opt/crabcc && git pull && \
>     sudo bash install/github-runner/install.sh --gc-only'
> done
> ```
>
> `--gc-only` auto-detects the runner's user + working dir from the
> existing `actions-runner.service`, so it targets the right account even
> when the runner runs as a non-root `deploy` user (pass `--user <name>` to
> override).
>
> New hosts get the timer automatically from a full `install.sh` run. For a
> one-off prune without SSH, trigger the on-demand `runner-gc` GitHub
> workflow (Actions → runner-gc → Run workflow), which runs the same script
> on whichever runner it lands on.

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
