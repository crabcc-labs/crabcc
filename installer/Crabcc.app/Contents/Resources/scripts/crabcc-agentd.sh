#!/bin/bash
# crabcc-agentd — long-running background service for the Crabcc.app bundle.
#
# Lifecycle:
#   * Spawned by launchd via the LaunchAgent at
#     ~/Library/LaunchAgents/com.crabcc.agentd.plist (RunAtLoad, KeepAlive).
#   * Survives sleep/wake/restart automatically — launchd reaps + relaunches.
#   * Receives SIGTERM on logout; SIGCHLD reaped via trap to avoid zombies.
#
# Work loop:
#   * Every $INTERVAL seconds, run `crabcc refresh --quiet` against any
#     repo recorded in ~/.crabcc/agent/repos.list (one absolute path per line).
#   * Sleeping is interruptible — SIGTERM/SIGINT short-circuits the wait.
#
# Design constraints:
#   * Minimal footprint: no daemons-of-daemons, no python, pure bash.
#   * Responsive: idle CPU is launchd's `sleep`; loop body is short.
#   * No zombie leakage: every spawn is `wait`-ed and SIGCHLD is trapped.

set -uo pipefail

LOG_DIR="$HOME/Library/Logs/Crabcc"
LOG_FILE="$LOG_DIR/agentd.log"
STATE_DIR="$HOME/.crabcc/agent"
REPOS_LIST="$STATE_DIR/repos.list"
PID_FILE="$STATE_DIR/agentd.pid"
INTERVAL="${CRABCC_AGENTD_INTERVAL:-300}"

mkdir -p "$LOG_DIR" "$STATE_DIR"

log() { printf '%s [agentd %d] %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$$" "$*" >> "$LOG_FILE"; }

# --- signal plumbing -------------------------------------------------------

shutdown=0
on_term() { log "received SIGTERM/SIGINT — shutting down"; shutdown=1; }
on_chld() { while wait -n 2>/dev/null; do :; done; }  # reap children, no zombies

trap on_term  TERM INT
trap on_chld  CHLD
trap 'log "exiting (rc=$?)"' EXIT

# --- single-instance guard -------------------------------------------------

if [[ -f "$PID_FILE" ]]; then
    prev=$(cat "$PID_FILE" 2>/dev/null || echo "")
    if [[ -n "$prev" ]] && kill -0 "$prev" 2>/dev/null; then
        log "another agentd already running (pid=$prev) — exiting"
        exit 0
    fi
fi
echo "$$" > "$PID_FILE"

log "agentd start (interval=${INTERVAL}s, pid=$$)"

# --- main loop -------------------------------------------------------------

CRABCC_BIN="${CRABCC_BIN:-$HOME/.cargo/bin/crabcc}"
[[ -x "$CRABCC_BIN" ]] || CRABCC_BIN="$(command -v crabcc 2>/dev/null || echo "")"

run_tick() {
    [[ -z "$CRABCC_BIN" ]] && { log "no crabcc binary on PATH — skipping tick"; return; }
    [[ -f "$REPOS_LIST" ]] || return
    while IFS= read -r repo; do
        [[ -z "$repo" || "$repo" == \#* ]] && continue
        [[ -d "$repo/.crabcc" ]] || continue
        (
            cd "$repo" || exit 0
            "$CRABCC_BIN" refresh --quiet >/dev/null 2>&1 || true
        ) &
        wait $!
    done < "$REPOS_LIST"
}

while [[ $shutdown -eq 0 ]]; do
    run_tick
    # Interruptible sleep: chunk into 5s bursts so SIGTERM lands within 5s.
    elapsed=0
    while [[ $shutdown -eq 0 && $elapsed -lt $INTERVAL ]]; do
        sleep 5
        elapsed=$((elapsed + 5))
    done
done

rm -f "$PID_FILE"
exit 0
