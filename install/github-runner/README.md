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
