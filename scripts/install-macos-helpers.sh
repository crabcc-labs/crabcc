#!/usr/bin/env bash
# install-macos-helpers.sh — register the Crabcc LaunchAgent against the
# binaries currently on PATH, without going through the .app/.dmg.
#
# Use cases:
#   * dev-machine install — you've `cargo install`'d crabcc and want the
#     background agent registered, but don't want to build/ship a DMG.
#   * CI smoke — exercise the LaunchAgent path on a runner.
#
# What it does (idempotent):
#   1. Ad-hoc codesigns ~/.cargo/bin/{crabcc,ccc} (Sequoia provenance fix).
#   2. Renders the bundled agentd plist template, pointing at a script that
#      runs the local repo's `installer/Crabcc.app/Contents/Resources/scripts/
#      crabcc-agentd.sh` (or installs a copy under ~/.crabcc/bin/ for
#      detachment from the working tree).
#   3. `launchctl bootstrap` into the user GUI domain.
#
# Usage:
#   scripts/install-macos-helpers.sh           # install
#   scripts/install-macos-helpers.sh --remove  # uninstall (bootout + rm)
#   scripts/install-macos-helpers.sh --status  # report state

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
LABEL="com.crabcc.agentd"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
PLIST_TEMPLATE="$REPO_ROOT/installer/Crabcc.app/Contents/Resources/com.crabcc.agentd.plist"
AGENTD_SCRIPT_SRC="$REPO_ROOT/installer/Crabcc.app/Contents/Resources/scripts/crabcc-agentd.sh"

INSTALL_ROOT="$HOME/.crabcc/bin"
AGENTD_SCRIPT_DST="$INSTALL_ROOT/crabcc-agentd.sh"
UID_NUM="$(id -u)"

log() { printf '[install-macos-helpers] %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

case "${1:-install}" in
  --remove|remove)
    /bin/launchctl bootout "gui/$UID_NUM/$LABEL" 2>/dev/null || true
    rm -f "$PLIST" "$AGENTD_SCRIPT_DST"
    log "uninstalled $LABEL"
    exit 0 ;;
  --status|status)
    [ -f "$PLIST" ] && log "plist: $PLIST" || log "plist: missing"
    /bin/launchctl print "gui/$UID_NUM/$LABEL" 2>/dev/null \
        | awk '/^\s*state =|pid =|last exit code/' || log "not loaded"
    exit 0 ;;
  --help|-h)
    sed -n '1,40p' "$0"; exit 0 ;;
esac

# --- preconditions --------------------------------------------------------

[ -f "$PLIST_TEMPLATE" ]    || die "missing $PLIST_TEMPLATE — run from the crabcc repo"
[ -f "$AGENTD_SCRIPT_SRC" ] || die "missing $AGENTD_SCRIPT_SRC"

# --- 1. ad-hoc sign binaries ----------------------------------------------

for b in crabcc ccc; do
    p="$HOME/.cargo/bin/$b"
    [ -x "$p" ] || continue
    /usr/bin/codesign --force --sign - "$p" 2>/dev/null \
        && log "codesigned $p" \
        || log "warn: codesign failed for $p"
done

# --- 2. install agentd script under stable path ---------------------------

mkdir -p "$INSTALL_ROOT"
install -m 0644 "$AGENTD_SCRIPT_SRC" "$AGENTD_SCRIPT_DST"
log "installed $AGENTD_SCRIPT_DST"

# --- 3. render and bootstrap LaunchAgent ----------------------------------

mkdir -p "$(dirname "$PLIST")"
sed -e "s|__APP_PATH__/Contents/Resources/scripts|$INSTALL_ROOT|g" \
    -e "s|__APP_PATH__|$INSTALL_ROOT|g" \
    -e "s|__HOME__|$HOME|g" \
    "$PLIST_TEMPLATE" > "$PLIST"
# Repoint the ProgramArguments to the bare script (we copied it above).
# The template points at __APP_PATH__/Contents/Resources/scripts/crabcc-agentd.sh;
# the sed above collapses that to $INSTALL_ROOT/crabcc-agentd.sh.
log "wrote $PLIST"

if /bin/launchctl print "gui/$UID_NUM/$LABEL" >/dev/null 2>&1; then
    /bin/launchctl bootout "gui/$UID_NUM/$LABEL" 2>/dev/null || true
fi

if /bin/launchctl bootstrap "gui/$UID_NUM" "$PLIST"; then
    /bin/launchctl enable "gui/$UID_NUM/$LABEL" 2>/dev/null || true
    /bin/launchctl kickstart -k "gui/$UID_NUM/$LABEL" 2>/dev/null || true
    log "agentd registered with launchd"
else
    die "launchctl bootstrap failed — check Console.app for crabcc.agentd entries"
fi

log "done — logs land in ~/Library/Logs/Crabcc/"
