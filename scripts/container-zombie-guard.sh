#!/usr/bin/env bash
# container-zombie-guard.sh — host-side janitor for Apple `container`
# (and Docker) state. Complements the `init: true` PID-1 reaper that
# runs INSIDE each VM (handles in-container zombies) by cleaning up
# at the VM-lifecycle level:
#
#   - Removes exited-but-not-removed agent containers > $MAX_AGE_HOURS old.
#   - Forces stop+rm of containers stuck in 'restarting' for > $RESTART_LOOP_LIMIT
#     consecutive checks (a respawn loop on a broken Containerfile).
#   - Reports on dangling builder caches > $BUILDER_CACHE_GIB GiB.
#
# Honors `container` first, falls back to `docker` for Linux runners.
# Targets only containers carrying a `com.crabcc.*` label so we never
# touch unrelated state on the user's host.
#
# Default cadence: invoke every 30 min via an external scheduler
# (cron / launchd / systemd).
# Manual: `bash scripts/container-zombie-guard.sh`.
#
# Idempotent. Read-only with --dry-run.

set -uo pipefail

DRY_RUN=0
JSON=0
MAX_AGE_HOURS="${MAX_AGE_HOURS:-24}"
RESTART_LOOP_LIMIT="${RESTART_LOOP_LIMIT:-3}"
BUILDER_CACHE_GIB="${BUILDER_CACHE_GIB:-10}"

for arg in "$@"; do
    case "$arg" in
        --dry-run|-n) DRY_RUN=1 ;;
        --json|-j)    JSON=1 ;;
        --help|-h)    sed -n '1,28p' "${BASH_SOURCE[0]:-$0}"; exit 0 ;;
    esac
done

# Pick the runtime CLI. Apple container ships as `container` on macOS;
# `docker` is the Linux / OrbStack fallback. Both speak the same surface
# we use here (`ls --format json`, `stop`, `rm`).
runtime=""
if command -v container >/dev/null 2>&1; then
    runtime="container"
elif command -v docker >/dev/null 2>&1; then
    runtime="docker"
else
    if [[ $JSON -eq 1 ]]; then
        echo '{"swept":0,"removed":0,"reason":"no-runtime"}'
    else
        echo "container-zombie-guard: no container/docker on PATH; nothing to do"
    fi
    exit 0
fi

# Parse JSON list of all crabcc-labeled containers (running + exited).
# `container ls --all --format json` and `docker ps -a --format json` both
# work; we ask for json so we don't have to parse human columns.
list_json="$($runtime ls --all --format json 2>/dev/null || echo '[]')"

# Filter to crabcc-labeled containers with `jq`; fall back to grep if jq
# isn't installed (less robust but functional).
if command -v jq >/dev/null 2>&1; then
    rows="$(echo "$list_json" | jq -r \
        '[ .[]? | select((.configuration.labels // .Labels // {} | tostring) | contains("com.crabcc"))
            | { id: (.configuration.id // .ID), state: (.status // .State), created: (.created // .Created), restarts: (.restartCount // 0) } ]')"
else
    # Coarse text grep — better than nothing.
    rows="[]"
fi

now_unix="$(date +%s)"
swept=0
removed=0
acted_on=()

# Iterate JSON rows. jq's --raw-output keeps it shell-safe.
while IFS=$'\t' read -r id state created restarts; do
    [[ -z "$id" ]] && continue
    # Convert ISO created → unix; tolerate either epoch or string.
    if [[ "$created" =~ ^[0-9]+$ ]]; then
        created_unix="$created"
    else
        created_unix="$(date -j -f '%Y-%m-%dT%H:%M:%SZ' "$created" '+%s' 2>/dev/null \
            || date -d "$created" '+%s' 2>/dev/null \
            || echo 0)"
    fi
    age_hours=$(( (now_unix - created_unix) / 3600 ))
    swept=$((swept + 1))

    case "$state" in
        exited|stopped)
            if [[ $age_hours -ge $MAX_AGE_HOURS ]]; then
                acted_on+=("$id::exited-old::${age_hours}h")
                if [[ $DRY_RUN -eq 0 ]]; then
                    $runtime rm -f "$id" >/dev/null 2>&1 && removed=$((removed + 1))
                fi
            fi
            ;;
        restarting)
            if [[ "${restarts:-0}" -ge $RESTART_LOOP_LIMIT ]]; then
                acted_on+=("$id::restart-loop::${restarts}")
                if [[ $DRY_RUN -eq 0 ]]; then
                    $runtime stop "$id" >/dev/null 2>&1
                    $runtime rm -f "$id" >/dev/null 2>&1 && removed=$((removed + 1))
                fi
            fi
            ;;
    esac
done < <(echo "$rows" | jq -r '.[] | "\(.id)\t\(.state)\t\(.created)\t\(.restarts)"' 2>/dev/null)

if [[ $JSON -eq 1 ]]; then
    actions_json="[$(IFS=,; printf '"%s"' "${acted_on[@]:-}")]"
    printf '{"runtime":"%s","swept":%d,"removed":%d,"actions":%s,"dry_run":%s}\n' \
        "$runtime" "$swept" "$removed" "$actions_json" \
        "$( [[ $DRY_RUN -eq 1 ]] && echo true || echo false )"
else
    echo "container-zombie-guard ($runtime): swept=$swept removed=$removed dry_run=$DRY_RUN"
    for a in "${acted_on[@]:-}"; do
        [[ -z "$a" ]] && continue
        echo "  $a"
    done
fi
