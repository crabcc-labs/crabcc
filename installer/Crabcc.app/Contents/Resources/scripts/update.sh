#!/bin/bash
# update.sh — fetch the latest crabcc from GitHub and run it.
#
# Wired to the menubar "Reinstall / Update…" action. Always fetches the
# most recent `scripts/bootstrap.sh` from the configured branch (default:
# main) so the update path can never get stuck on a stale local copy.
#
# Resolution order for the fetch:
#   1. `gh api`        — preferred. Auth flows + rate limits handled.
#   2. `curl` raw URL  — fallback when gh isn't installed or not authed.
#
# Once bootstrap.sh is on disk, this script execs it with the same env
# variables that were exported by the parent (no flags drop-through —
# the menubar action runs the canonical update path: code + aliases +
# skills + commands + LaunchAgent).
#
# Idempotent. Surfaces a final stdout summary so the menubar's "done"
# alert can show a one-line result.

set -uo pipefail

REPO_OWNER="${CRABCC_REPO_OWNER:-peterlodri-sec}"
REPO_NAME="${CRABCC_REPO_NAME:-crabcc}"
BRANCH="${CRABCC_BRANCH:-main}"
RAW_URL="https://raw.githubusercontent.com/$REPO_OWNER/$REPO_NAME/$BRANCH/scripts/bootstrap.sh"

LOG_DIR="$HOME/Library/Logs/Crabcc"
LOG_FILE="$LOG_DIR/update.log"
mkdir -p "$LOG_DIR"

log() { printf '[update %s] %s\n' "$(date -u '+%H:%M:%SZ')" "$*"; }
die() { log "ERROR: $*"; exit 1; }

TMP_DIR="$(mktemp -d -t crabcc-update.XXXXXX)"
trap 'rm -rf "$TMP_DIR"' EXIT
BOOTSTRAP="$TMP_DIR/bootstrap.sh"

# --- 1. fetch bootstrap.sh -------------------------------------------------

fetched=0
if command -v gh >/dev/null 2>&1; then
    log "fetching scripts/bootstrap.sh via gh api ($BRANCH)"
    if gh api -H 'Accept: application/vnd.github.v3.raw' \
           "/repos/$REPO_OWNER/$REPO_NAME/contents/scripts/bootstrap.sh?ref=$BRANCH" \
           > "$BOOTSTRAP" 2>>"$LOG_FILE"; then
        fetched=1
        log "fetched via gh ($(wc -c < "$BOOTSTRAP") bytes)"
    else
        log "gh api failed — falling back to curl"
    fi
fi

if [[ $fetched -eq 0 ]]; then
    log "fetching via curl: $RAW_URL"
    if curl -fsSL "$RAW_URL" -o "$BOOTSTRAP" 2>>"$LOG_FILE"; then
        fetched=1
        log "fetched via curl ($(wc -c < "$BOOTSTRAP") bytes)"
    else
        die "could not download bootstrap.sh from $RAW_URL — check network + repo visibility"
    fi
fi

# Sanity-check the download — bootstrap.sh starts with `#!/usr/bin/env bash`.
head -1 "$BOOTSTRAP" | grep -q '^#!.*bash' \
    || die "downloaded file does not look like a bash script (got: $(head -1 "$BOOTSTRAP"))"

# --- 2. run bootstrap.sh ---------------------------------------------------

log "running bootstrap.sh"
chmod 0755 "$BOOTSTRAP"

# Pass-through env: bootstrap.sh respects CRABCC_HOME / CRABCC_REPO_URL.
# We invoke with --branch so the cloned tree matches the bootstrap source.
bash "$BOOTSTRAP" --branch "$BRANCH"
rc=$?

if [[ $rc -eq 0 ]]; then
    log "update complete (rc=0)"
else
    log "bootstrap failed (rc=$rc) — see $LOG_FILE"
fi

# --- 3. refresh agentd if present -----------------------------------------
# After binaries upgrade, kick the LaunchAgent so it picks up new behavior.

if /bin/launchctl print "gui/$(id -u)/com.crabcc.agentd" >/dev/null 2>&1; then
    /bin/launchctl kickstart -k "gui/$(id -u)/com.crabcc.agentd" 2>/dev/null \
        && log "kickstarted com.crabcc.agentd"
fi

exit $rc
