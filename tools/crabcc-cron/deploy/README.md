# Deploying crabcc-cron to Hetzner

Idempotent install of the `crabcc-cron` dispatcher onto a Hetzner box. The
installer is safe to re-run after `git pull` — it re-symlinks `/opt/crabcc-cron`,
re-installs the crontab, and leaves `/etc/crabcc-cron/env` alone.

## Prerequisites

Target box runs Debian/Ubuntu with a `deploy` user/group. Install
runtime deps once:

```bash
sudo apt-get update
sudo apt-get install -y jq curl gh shellcheck python3
```

`opencode` is installed separately (its own binary; see opencode docs).

## Clone the repo

```bash
sudo git clone https://github.com/peterlodri-sec/crabcc.git /srv/repos/crabcc
```

If you keep the checkout somewhere else, export `REPO_ROOT` before
running the installer.

## Install

```bash
sudo bash /srv/repos/crabcc/tools/crabcc-cron/deploy/install.sh
```

The installer:

1. Symlinks `/opt/crabcc-cron` → `/srv/repos/crabcc/tools/crabcc-cron`.
2. Creates `/etc/crabcc-cron/` (mode `750`, `root:deploy`).
3. Copies `env.example` → `/etc/crabcc-cron/env` (mode `600`,
   `root:deploy`) if no env file exists yet. Existing env is preserved.
4. Copies `crabcc-cron.cron` → `/etc/cron.d/crabcc-cron`.
5. Creates state dirs `/opt/crabcc-cron-state/` and
   `/srv/cron-agents/oss-fix/` owned by `deploy:deploy`.
6. Runs `shellcheck` over `bin/`, `jobs/`, and `lib/` before declaring
   success.

## Edit secrets

On a fresh install the env file holds placeholder `*_REDACTED` values.
Replace them before the next cron tick:

```bash
sudo -u deploy vi /etc/crabcc-cron/env
```

Keep `OSS_FIX_DRY_RUN=1` for at least the first week of deployment.

## Tail logs

Each cron run pipes stdout/stderr through `systemd-cat -t
crabcc-cron-oss-fix`, so logs land in the journal:

```bash
sudo journalctl -t crabcc-cron-oss-fix -f
```

## Updating

```bash
cd /srv/repos/crabcc && git pull && sudo bash tools/crabcc-cron/deploy/install.sh
```

The symlink layout means `git pull` alone updates the scripts; re-running
the installer is only needed when the crontab template or env example
changes.

## Disabling

Stop cron from firing the job:

```bash
sudo rm /etc/cron.d/crabcc-cron
```

State, env, and the `/opt/crabcc-cron` symlink are left in place so the
job can be re-enabled by re-running the installer.
