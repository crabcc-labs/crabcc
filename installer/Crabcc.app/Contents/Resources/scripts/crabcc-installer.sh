#!/bin/bash
# crabcc-installer — one-shot launcher embedded inside Crabcc.app.
#
# Behavior:
#   1. Show an AppleScript confirmation dialog.
#   2. Run Resources/install.sh (which copies binaries, codesigns, links
#      skills/commands, runs install-aliases, registers LaunchAgent).
#   3. Stream the install log into a final "done / failed" alert.
#
# Designed to run from /Applications/Crabcc.app on first launch. Idempotent —
# re-running re-installs cleanly.

set -uo pipefail

# Resources/scripts/ → Resources/ is parent.
SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
RESOURCES="$(cd "$SCRIPTS_DIR/.." && pwd)"
LOG_DIR="$HOME/Library/Logs/Crabcc"
LOG_FILE="$LOG_DIR/installer.log"
mkdir -p "$LOG_DIR"

osascript_dialog() {
    osascript <<APPLESCRIPT 2>/dev/null
display dialog "$1" buttons {"Cancel", "Install"} default button "Install" with title "Crabcc Installer" with icon note
APPLESCRIPT
}

osascript_alert() {
    local title="$1"; local body="$2"; local icon="${3:-note}"
    osascript <<APPLESCRIPT >/dev/null 2>&1
display alert "$title" message "$body" as $icon
APPLESCRIPT
}

# Confirm with the user. The osascript exits non-zero on Cancel.
if ! osascript_dialog "Install Crabcc CLI, skills, slash commands, shell aliases, and the background agent?"; then
    exit 0
fi

{
    echo "=== crabcc install $(date -u '+%Y-%m-%dT%H:%M:%SZ') ==="
    bash "$RESOURCES/scripts/install.sh"
    rc=$?
    echo "=== exit $rc ==="
    exit $rc
} > "$LOG_FILE" 2>&1

rc=$?
if [[ $rc -eq 0 ]]; then
    osascript_alert "Crabcc installed" "CLI in ~/.cargo/bin · skills + commands linked · agent registered as LaunchAgent." informational
else
    osascript_alert "Crabcc install failed" "Exit code $rc — see $LOG_FILE" critical
fi
exit $rc
