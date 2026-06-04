#!/usr/bin/env bash
# Host-local disk reclamation for a Hetzner self-hosted runner.
#
# Run on a schedule by actions-runner-gc.timer (installed by install.sh —
# this is what guarantees *every* host in the fleet self-cleans, since a
# single GitHub Actions cron only lands on one runner), and on demand by
# the `runner-gc` GitHub workflow.
#
# Prunes only unused / regenerable data — Docker images/build cache, cargo
# registry sources, sccache objects, tool-cache, apt + journald, stale temp,
# and orphaned job checkout dirs.
#
# Docker/apt/journald/old-temp pruning only ever touch unused data, so they
# are safe during a build; cargo/sccache pruning is skipped while a job is
# running (see runner_busy) to avoid deleting sources out from under a
# concurrent compile.
#
# Auto-escalation: if root fs is still ≥80% after a normal (non-deep) Docker
# prune, an aggressive deep prune fires automatically — no manual --deep
# needed for routine full-disk situations.
#
# Usage: runner-gc.sh [--deep]   (--deep forces aggressive Docker prune)
set -uo pipefail

DEEP=0
THRESHOLD=""  # empty = always run; set via --if-above N to skip when disk is low
while [ $# -gt 0 ]; do
  case "$1" in
    --deep)     DEEP=1;           shift   ;;
    --if-above) THRESHOLD="$2";   shift 2 ;;
    *) echo "[runner-gc] unknown arg: $1" >&2; exit 1 ;;
  esac
done
log() { echo "[runner-gc] $*"; }

# Where install.sh placed the dedicated cache volume (or plain directory).
# Passed via Environment= in the GC service unit; fall back gracefully when
# the script is invoked manually without that env set.
CACHE_BASE="${RUNNER_CACHE_BASE:-/var/runner-data}"

# Effective CARGO_HOME: prefer the env var the runner service exports, then
# the cache volume path, then the default ~/.cargo.
EFFECTIVE_CARGO_HOME="${CARGO_HOME:-${CACHE_BASE}/cargo}"

# A job in progress spawns a `Runner.Worker` process. Used to hold off
# cargo/sccache prunes while the host is compiling.
runner_busy() { pgrep -f 'Runner\.Worker' >/dev/null 2>&1; }

# Current root-fs fill percentage as an integer (empty string on failure).
root_pct() { df --output=pcent / 2>/dev/null | tail -1 | tr -dc '0-9'; }

# Early exit when called by the disk-watchdog timer and disk is healthy.
# The 15-min watchdog uses --if-above 75; the 4h full-GC timer omits it.
if [ -n "${THRESHOLD:-}" ]; then
  THRESHOLD_CHECK="$(root_pct)"
  if [ -n "${THRESHOLD_CHECK:-}" ] && [ "$THRESHOLD_CHECK" -lt "$THRESHOLD" ]; then
    log "disk at ${THRESHOLD_CHECK}% — below threshold ${THRESHOLD}%, nothing to do"
    exit 0
  fi
  log "disk at ${THRESHOLD_CHECK}% ≥ threshold ${THRESHOLD}% — running GC"
fi

log "host $(hostname) — disk before:"
df -h / "${CACHE_BASE}" 2>/dev/null || df -h / 2>/dev/null || true

# ── Docker: unused images / build cache / stopped containers ─────────────
prune_docker() {
  local deep="$1"
  if ! command -v docker >/dev/null 2>&1 || ! docker version >/dev/null 2>&1; then
    log "docker not present / daemon unreachable; skipping"
    return
  fi
  if [ "$deep" = 1 ]; then
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
}

prune_docker "$DEEP"

# Auto-escalation: if root fs is still ≥80% after a normal prune, escalate
# to deep — but only when the runner is idle.  docker system prune -af
# removes ALL unused images and build cache regardless of age; an image
# built earlier in an active job but not yet attached to a container is
# eligible, so a later step's `docker run` can fail.
# Skipped when --deep was already passed (prune_docker already ran deep).
if [ "$DEEP" = 0 ]; then
  USE_MID="$(root_pct)"
  if [ -n "${USE_MID:-}" ] && [ "$USE_MID" -ge 80 ]; then
    if runner_busy; then
      log "root fs at ${USE_MID}% but runner busy — deferring deep Docker prune until idle"
    else
      log "root fs at ${USE_MID}% after normal Docker prune — auto-escalating to deep"
      prune_docker 1
    fi
  fi
fi

# ── Cargo: regenerable registry sources + git checkouts ──────────────────
# Only when the runner is idle — a concurrent compile may be reading a dep
# whose mtime is >7d (Cargo never refreshes it), and deleting it mid-build
# fails the job.
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

# ── sccache: prune objects older than 14 days ─────────────────────────────
# sccache stores objects as flat files; entries not accessed in 14d are stale
# enough to be safe to delete (they'll be rebuilt on the next cache miss).
# Skip while a job runs — we don't want to evict a cache hit mid-build.
SCCACHE_DIR_PATH="${SCCACHE_DIR:-${CACHE_BASE}/sccache}"
if [ -d "$SCCACHE_DIR_PATH" ] && ! runner_busy; then
  find "${SCCACHE_DIR_PATH}" -mindepth 1 -atime +14 -delete 2>/dev/null || true
  log "sccache: pruned objects not accessed in 14d from ${SCCACHE_DIR_PATH}"
fi

# ── tool-cache: prune downloaded toolchain entries older than 30 days ──────
# Guard with runner_busy: setup-* actions resolve toolchains from this dir
# throughout a job; deleting an entry while a step is still running breaks it.
TOOL_CACHE_PATH="${RUNNER_TOOL_CACHE:-${CACHE_BASE}/tool-cache}"
if [ -d "$TOOL_CACHE_PATH" ] && ! runner_busy; then
  find "${TOOL_CACHE_PATH}" -mindepth 1 -maxdepth 2 -mtime +30 -exec rm -rf {} + 2>/dev/null || true
  log "tool-cache: pruned entries older than 30d from ${TOOL_CACHE_PATH}"
fi

# ── Cargo target dirs on the cache volume ─────────────────────────────────
# CARGO_TARGET_DIR is relocated onto the data volume (see install.sh) and
# namespaced per runner under ${CACHE_BASE}/target/<runner>. Build artifacts
# are fully regenerable, so prune per-runner trees not touched in 7 days.
# Skipped mid-build — the live target dir would be deleted out from under cargo.
TARGET_BASE="${RUNNER_TARGET_BASE:-${CACHE_BASE}/target}"
if [ -d "$TARGET_BASE" ] && ! runner_busy; then
  # Prune a per-runner target dir only when its WHOLE subtree is cold (no file
  # touched in 7d). Keying on the top-level dir's own mtime would wrongly evict
  # a steadily-built cache whose layout (debug/, release/) rarely changes, which
  # only updates the parent dir's mtime when entries are added/removed.
  for d in "$TARGET_BASE"/*/; do
    [ -d "$d" ] || continue
    if [ -z "$(find "$d" -type f -newermt '7 days ago' -print -quit 2>/dev/null)" ]; then
      rm -rf "$d" 2>/dev/null || true
      log "cargo target: pruned cold dir ${d}"
    fi
  done
fi

# ── System caches: apt + journald ─────────────────────────────────────────
sudo apt-get clean 2>/dev/null || true
command -v journalctl >/dev/null 2>&1 && sudo journalctl --vacuum-time=2d 2>/dev/null || true

# ── Stale temp from completed jobs ────────────────────────────────────────
# Clean both the system /tmp and the dedicated cache volume tmp dir.
for tmp_dir in /tmp "${CACHE_BASE}/tmp"; do
  [ -d "$tmp_dir" ] &&
    find "$tmp_dir" -mindepth 1 -maxdepth 1 -mtime +1 -exec rm -rf {} + 2>/dev/null || true
done
for tmp in "${RUNNER_TEMP:-}" "${RUNNER_GC_WORK_TEMP:-}" "$HOME/actions-runner/_work/_temp"; do
  [ -n "$tmp" ] && [ -d "$tmp" ] &&
    find "$tmp" -mindepth 1 -mtime +1 -exec rm -rf {} + 2>/dev/null || true
done

# ── Orphaned job checkout dirs ─────────────────────────────────────────────
# Actions cleans up _work after each job, but dirs from abruptly-killed jobs
# survive indefinitely. Prune top-level dirs in _work older than 2 days.
# Skipped while a job runs to avoid deleting the live workspace.
if ! runner_busy; then
  WORK_DIR="$HOME/actions-runner/_work"
  if [ -d "$WORK_DIR" ]; then
    find "$WORK_DIR" -mindepth 1 -maxdepth 1 -mtime +2 -exec rm -rf {} + 2>/dev/null || true
    log "orphaned _work dirs older than 2d pruned from ${WORK_DIR}"
  fi
fi

log "disk after:"
df -h / "${CACHE_BASE}" 2>/dev/null || df -h / 2>/dev/null || true

USE="$(root_pct)"
log "root filesystem usage after GC: ${USE:-?}%"
if [ -n "${USE:-}" ] && [ "$USE" -ge 85 ]; then
  log "WARNING: root fs still ${USE}% full after GC — manual intervention may be needed (try --deep or add disk)"
fi

# Report cache volume usage if it's a separate mount.
if mountpoint -q "${CACHE_BASE}" 2>/dev/null; then
  CACHE_USE=$(df --output=pcent "${CACHE_BASE}" 2>/dev/null | tail -1 | tr -dc '0-9')
  log "cache volume (${CACHE_BASE}) usage: ${CACHE_USE:-?}%"
  if [ -n "${CACHE_USE:-}" ] && [ "$CACHE_USE" -ge 80 ]; then
    log "WARNING: cache volume at ${CACHE_USE}% — sccache/cargo growth may need attention"
  fi
fi
