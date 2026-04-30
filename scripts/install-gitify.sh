#!/usr/bin/env bash
# install-gitify.sh — check for + install gitify-app/gitify, the
# open-source macOS menubar GitHub-notifications app.
#
#   https://github.com/gitify-app/gitify  (AGPL-3.0)
#
# Pairs naturally with crabcc's own menubar (issue #107):
#   - crabcc menubar    : local agent / index / backup state
#   - gitify menubar    : remote PR / issue / mention / review state
# Both sit in the macOS menubar, neither competes for screen real
# estate, and operators get a one-glance view of "what's happening
# locally + remotely on my crabcc work" without alt-tabbing to the
# browser or terminal.
#
# Usage:
#   bash scripts/install-gitify.sh                  # check + install
#   bash scripts/install-gitify.sh --check          # report only
#   bash scripts/install-gitify.sh --launch         # open after install
#   bash scripts/install-gitify.sh --json           # machine-readable
#
# Idempotent. macOS-only — exits cleanly with a "skipped" status on
# Linux / Windows.

set -uo pipefail

CHECK_ONLY=0
LAUNCH=0
JSON=0
for arg in "$@"; do
    case "$arg" in
        --check|-c)   CHECK_ONLY=1 ;;
        --launch|-l)  LAUNCH=1 ;;
        --json|-j)    JSON=1 ;;
        --help|-h)    sed -n '1,28p' "${BASH_SOURCE[0]:-$0}"; exit 0 ;;
    esac
done

if [[ -t 1 ]] && [[ $JSON -eq 0 ]]; then
    GRN='\033[32m'; YEL='\033[33m'; CYN='\033[36m'; OFF='\033[0m'
else
    GRN=''; YEL=''; CYN=''; OFF=''
fi

emit_json() {
    printf '{"installed":%s,"path":"%s","action":"%s","reason":"%s"}\n' \
        "$1" "$2" "$3" "${4:-}"
}

# --- platform gate --------------------------------------------------------

if [[ "$(uname -s)" != "Darwin" ]]; then
    if [[ $JSON -eq 1 ]]; then
        emit_json false "" "skipped" "non-macOS host"
    else
        printf "%bgitify is macOS-only — skipping%b\n" "$YEL" "$OFF"
    fi
    exit 0
fi

# --- detect existing install ---------------------------------------------

GITIFY_APP="/Applications/Gitify.app"
[[ -d "$HOME/Applications/Gitify.app" ]] && GITIFY_APP="$HOME/Applications/Gitify.app"

if [[ -d "$GITIFY_APP" ]]; then
    version="$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' "$GITIFY_APP/Contents/Info.plist" 2>/dev/null || echo "?")"
    if [[ $JSON -eq 1 ]]; then
        emit_json true "$GITIFY_APP" "already-installed" "version $version"
    else
        printf "%b✓ Gitify %s installed at %s%b\n" "$GRN" "$version" "$GITIFY_APP" "$OFF"
    fi
    if [[ $LAUNCH -eq 1 ]]; then
        open "$GITIFY_APP"
    fi
    exit 0
fi

# --- check-only path ------------------------------------------------------

if [[ $CHECK_ONLY -eq 1 ]]; then
    if [[ $JSON -eq 1 ]]; then
        emit_json false "" "missing" "not installed; re-run without --check to brew install"
    else
        printf "%b! Gitify not installed%b\n" "$YEL" "$OFF"
        printf "  install: brew install --cask gitify\n"
        printf "  or download: https://github.com/gitify-app/gitify/releases/latest\n"
    fi
    exit 1
fi

# --- install via Homebrew (preferred) -------------------------------------

if ! command -v brew >/dev/null 2>&1; then
    if [[ $JSON -eq 1 ]]; then
        emit_json false "" "blocked" "Homebrew not on PATH; install brew first"
    else
        printf "%b! Homebrew not found.%b\n" "$YEL" "$OFF"
        printf "  install brew (https://brew.sh) or download Gitify directly:\n"
        printf "  https://github.com/gitify-app/gitify/releases/latest\n"
    fi
    exit 1
fi

[[ $JSON -eq 0 ]] && printf "%b· brew install --cask gitify%b\n" "$CYN" "$OFF"
if brew install --cask gitify >/dev/null 2>&1; then
    # Re-detect after install — homebrew sometimes lands the .app in
    # /Applications, sometimes /opt/homebrew/Caskroom (depending on
    # the cask version). Resolve the actual path before reporting.
    for cand in "/Applications/Gitify.app" "$HOME/Applications/Gitify.app"; do
        [[ -d "$cand" ]] && GITIFY_APP="$cand" && break
    done
    if [[ $JSON -eq 1 ]]; then
        emit_json true "$GITIFY_APP" "installed" "brew cask"
    else
        printf "%b✓ Gitify installed at %s%b\n" "$GRN" "$GITIFY_APP" "$OFF"
        printf "  next: launch + sign in at %sgithub.com → personal access token%s\n" "$CYN" "$OFF"
    fi
    if [[ $LAUNCH -eq 1 ]] && [[ -d "$GITIFY_APP" ]]; then
        open "$GITIFY_APP"
    fi
    exit 0
else
    if [[ $JSON -eq 1 ]]; then
        emit_json false "" "failed" "brew install --cask gitify returned non-zero"
    else
        printf "%b✗ brew install --cask gitify failed.%b\n" "$YEL" "$OFF"
        printf "  fallback: download .dmg from https://github.com/gitify-app/gitify/releases/latest\n"
    fi
    exit 1
fi
