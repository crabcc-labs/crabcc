#!/usr/bin/env bash
# set-local-dev-envs.sh — set + report the dev-side environment for
# crabcc work. Sourceable from a shell so the env vars stick:
#
#   source scripts/set-local-dev-envs.sh           # set + print
#   source scripts/set-local-dev-envs.sh --quiet   # set, no report
#   bash scripts/set-local-dev-envs.sh --print     # report only, do not set
#
# When sourced, exports:
#   CRABCC_REPO         absolute path to the repo root
#   CRABCC_VERSION      workspace version from Cargo.toml
#   CRABCC_BIN          path to ~/.cargo/bin/crabcc
#   CRABCC_DB           path to .crabcc/index.db (per-repo)
#   CRABCC_INTERNAL_DB  path to ~/.crabcc/_internal.db (singleton)
#   CRABCC_LOGS         path to ~/Library/Logs/Crabcc/ (macOS)
#   PATH                $HOME/.cargo/bin prepended (idempotent)
#
# When run, prints a single-screen status block covering: repo state,
# git revision, branch, dirty flag, build provenance, and the
# LaunchAgent / DB state on macOS — the same surface `crabcc manager
# status` reports, but as a one-shot snapshot you can copy-paste into
# bug reports.

# Detect sourced vs invoked. When sourced, $0 is the parent shell.
__sourced=0
[[ "${BASH_SOURCE[0]:-}" != "${0:-}" ]] && __sourced=1

QUIET=0
PRINT_ONLY=0
for arg in "$@"; do
    case "$arg" in
        --quiet|-q)  QUIET=1 ;;
        --print|-p)  PRINT_ONLY=1 ;;
        --help|-h)   sed -n '1,28p' "${BASH_SOURCE[0]:-$0}"; return 0 2>/dev/null || exit 0 ;;
        *) printf 'unknown flag: %s\n' "$arg" >&2; return 2 2>/dev/null || exit 2 ;;
    esac
done

# --- resolve repo root -----------------------------------------------------

__here="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"

# --- compute env values ----------------------------------------------------

__version="$(awk -F'"' '/^version[[:space:]]*=/ {print $2; exit}' "$__here/Cargo.toml" 2>/dev/null)"
__commit="$(git -C "$__here" rev-parse --short HEAD 2>/dev/null || echo "unknown")"
__branch="$(git -C "$__here" rev-parse --abbrev-ref HEAD 2>/dev/null || echo "unknown")"
__dirty="clean"
if [[ -n "$(git -C "$__here" status --porcelain 2>/dev/null)" ]]; then
    __dirty="dirty"
fi

__crabcc_bin="$HOME/.cargo/bin/crabcc"
__crabcc_version=""
[[ -x "$__crabcc_bin" ]] && __crabcc_version="$("$__crabcc_bin" --version 2>/dev/null)"

__os="$(uname -s)"
__arch="$(uname -m)"
__logs_dir="$HOME/Library/Logs/Crabcc"
[[ "$__os" != "Darwin" ]] && __logs_dir="$HOME/.cache/crabcc/logs"

# --- export (skip when --print) -------------------------------------------

if [[ $PRINT_ONLY -eq 0 ]]; then
    export CRABCC_REPO="$__here"
    export CRABCC_VERSION="$__version"
    export CRABCC_BIN="$__crabcc_bin"
    export CRABCC_DB="$__here/.crabcc/index.db"
    export CRABCC_INTERNAL_DB="$HOME/.crabcc/_internal.db"
    export CRABCC_LOGS="$__logs_dir"
    case ":$PATH:" in
        *:"$HOME/.cargo/bin":*) ;;
        *) export PATH="$HOME/.cargo/bin:$PATH" ;;
    esac
fi

# --- report ---------------------------------------------------------------

[[ $QUIET -eq 1 ]] && return 0 2>/dev/null
[[ $QUIET -eq 1 ]] && exit 0

if [[ -t 1 ]]; then BOLD='\033[1m'; DIM='\033[2m'; OFF='\033[0m'; else BOLD=''; DIM=''; OFF=''; fi

printf "${BOLD}crabcc dev environment${OFF}\n"
printf "  ${DIM}repo${OFF}              %s\n" "$__here"
printf "  ${DIM}version${OFF}           %s\n" "${__version:-?}"
printf "  ${DIM}git${OFF}               %s @ %s (%s)\n" "$__branch" "$__commit" "$__dirty"
printf "  ${DIM}os/arch${OFF}           %s/%s\n" "$__os" "$__arch"
printf "  ${DIM}crabcc bin${OFF}        %s\n" "$__crabcc_bin"
printf "  ${DIM}crabcc --version${OFF}  %s\n" "${__crabcc_version:-(not installed)}"
printf "  ${DIM}index db${OFF}          %s\n" "$__here/.crabcc/index.db"
printf "  ${DIM}_internal db${OFF}      %s\n" "$HOME/.crabcc/_internal.db"
printf "  ${DIM}logs${OFF}              %s\n" "$__logs_dir"

if [[ "$__os" == "Darwin" ]]; then
    printf "\n${BOLD}macOS LaunchAgents${OFF}\n"
    for label in com.crabcc.manager com.crabcc.menubar com.crabcc.agentd com.crabcc.agent-guard; do
        plist="$HOME/Library/LaunchAgents/$label.plist"
        if [[ -f "$plist" ]]; then
            state=$(/bin/launchctl print "gui/$(id -u)/$label" 2>/dev/null \
                | awk '/state =/ {print $3; exit}')
            printf "  ${DIM}%s${OFF}  %s\n" "$label" "${state:-not loaded}"
        else
            printf "  ${DIM}%s${OFF}  (plist missing)\n" "$label"
        fi
    done
fi

if command -v "$__crabcc_bin" >/dev/null 2>&1 && [[ -f "$HOME/.crabcc/_internal.db" ]]; then
    printf "\n${BOLD}manager status${OFF} ${DIM}(crabcc manager status)${OFF}\n"
    "$__crabcc_bin" manager status 2>/dev/null | sed 's/^/  /'
fi

[[ $__sourced -eq 1 ]] && return 0
exit 0
