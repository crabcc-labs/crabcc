#!/usr/bin/env bash
# Host-local disk reclamation for a Hetzner self-hosted runner.
#
# Run on a schedule by actions-runner-gc.timer (installed by install.sh —
# this is what guarantees *every* host in the fleet self-cleans, since a
# single GitHub Actions cron only lands on one runner), and on demand by
# the `runner-gc` GitHub workflow.
#
# Prunes only unused / regenerable data — Docker images/build cache, cargo
# registry sources, apt + journald, and stale temp from completed jobs.
# Docker/apt/journald/old-temp pruning only ever touch unused data, so they
# are safe during a build; the cargo-cache prune is mtime-based and Cargo
# does NOT bump mtimes on in-use deps, so it is skipped while a job is
# running (see runner_busy) to avoid deleting sources out from under a
# concurrent compile.
#
# Usage: runner-gc.sh [--deep]   (--deep ignores age filters on Docker)
set -uo pipefail

DEEP=0
[ "${1:-}" = "--deep" ] && DEEP=1
log() { echo "[runner-gc] $*"; }

# Where install.sh placed the dedicated cache volume (or plain directory).
# Passed via Environment= in the GC service unit; fall back gracefully when
# the script is invoked manually without that env set.
CACHE_BASE="${RUNNER_CACHE_BASE:-/var/runner-data}"

# Effective CARGO_HOME: prefer the env var the runner service exports, then
# the cache volume path, then the default ~/.cargo.
EFFECTIVE_CARGO_HOME="${CARGO_HOME:-${CACHE_BASE}/cargo}"

# A job in progress spawns a `Runner.Worker` process. Used to hold off the
# cargo-cache prune while the host is compiling.
runner_busy() { pgrep -f 'Runner\.Worker' >/dev/null 2>&1; }

log "host $(hostname) — disk before:"
df -h / "${CACHE_BASE}" 2>/dev/null || df -h / 2>/dev/null || true

# ── Docker: unused images / build cache / stopped containers ───────────
if command -v docker >/dev/null 2>&1 && docker version >/dev/null 2>&1; then
  if [ "$DEEP" = 1 ]; then
    log "deep prune: all unused images + build cache"
    docker system prune -af || true
    docker builder prune -af || true
  else
    # Only *unused* resources are eligible; keep anything from the last
    # 48h so a running/recent build's layers survive.
    docker container prune -f || true
    docker image prune -af --filter "until=48h" || true
    docker builder prune -af --filter "until=48h" || true
    docker network prune -f || true
  fi
else
  log "docker not present / daemon unreachable; skipping"
fi

# ── Cargo: regenerable registry sources + git checkouts ────────────────
# Only when the runner is idle — a concurrent compile may be reading a dep
# whose mtime is >7d (Cargo never refreshes it), and deleting it mid-build
# fails the job. The disk hog is Docker (pruned above) anyway; cargo is a
# bonus reclaim when safe.
if runner_busy; then
  log "runner busy (job in progress) — skipping cargo cache prune"
else
  for d in \
      "${EFFECTIVE_CARGO_HOME}/registry/src" \
      "${EFFECTIVE_CARGO_HOME}/registry/cache"; do
    [ -d "$d" ] && find "$d" -mindepth 1 -maxdepth 1 -mtime +7 -exec rm -rf {} + 2>/dev/null || true
  done
  [ -d "${EFFECTIVE_CARGO_HOME}/git/checkouts" ] &&
    find "${EFFECTIVE_CARGO_HOME}/git/checkouts" \
      -mindepth 1 -maxdepth 1 -mtime +14 -exec rm -rf {} + 2>/dev/null || true
fi

# ── System caches: apt + journald ──────────────────────────────────────
sudo apt-get clean 2>/dev/null || true
command -v journalctl >/dev/null 2>&1 && sudo journalctl --vacuum-time=2d 2>/dev/null || true

# ── Stale temp from completed jobs (current job's files are <1d old) ────
# Clean both the system /tmp and the dedicated cache volume tmp dir.
for tmp_dir in /tmp "${CACHE_BASE}/tmp"; do
  [ -d "$tmp_dir" ] &&
    find "$tmp_dir" -mindepth 1 -maxdepth 1 -mtime +1 -exec rm -rf {} + 2>/dev/null || true
done
for tmp in "${RUNNER_TEMP:-}" "${RUNNER_GC_WORK_TEMP:-}" "$HOME/actions-runner/_work/_temp"; do
  [ -n "$tmp" ] && [ -d "$tmp" ] &&
    find "$tmp" -mindepth 1 -mtime +1 -exec rm -rf {} + 2>/dev/null || true
done

log "disk after:"
df -h / "${CACHE_BASE}" 2>/dev/null || df -h / 2>/dev/null || true
USE=$(df --output=pcent / 2>/dev/null | tail -1 | tr -dc '0-9')
log "root filesystem usage after GC: ${USE:-?}%"
if [ -n "${USE:-}" ] && [ "$USE" -ge 85 ]; then
  log "WARNING: root fs still ${USE}% full after GC — consider 'runner-gc.sh --deep' or a larger disk"
fi

# Report cache volume usage if it's a separate mount.
if mountpoint -q "${CACHE_BASE}" 2>/dev/null; then
  CACHE_USE=$(df --output=pcent "${CACHE_BASE}" 2>/dev/null | tail -1 | tr -dc '0-9')
  log "cache volume (${CACHE_BASE}) usage: ${CACHE_USE:-?}%"
fi
