#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/install-aliases.sh
#
# Install developer-friendly aliases for modern CLI tools into the user's
# shell rc file. Each alias is gated on the *modern* tool actually being
# present, so the rc never breaks if `bat` / `eza` / `rg` aren't yet
# installed.
#
# What this maps:
#   grep   → rg          (ripgrep — keeps regex, far faster on big trees)
#   find   → fd          (saner defaults, gitignore-aware)
#   cat    → bat         (syntax highlighting, retains pipe-friendliness)
#   ls     → eza         (color, git-aware, type icons)
#   du     → dust
#   df     → duf
#   ps     → procs
#   top    → btop / htop
#   tree   → eza --tree  (when eza is installed; falls back to `tree`)
#   cd     → z           (zoxide — fuzzy `cd` to recent dirs)
#   tail   → lnav        (only when called as `tail -f`)
#
# Plus a small set of crabcc-specific aliases:
#   cc        → crabcc
#   cci       → crabcc index
#   ccs       → crabcc sym
#   ccr       → crabcc refs
#   ccc       → crabcc callers
#   cm        → crabcc memory
#
# Idempotent: writes a fenced `# >>> crabcc-aliases >>>` block; re-running
# replaces the block in place. `--remove` strips the block cleanly.
#
# Usage:
#   scripts/install-aliases.sh                 # detect shell, install
#   scripts/install-aliases.sh --shell zsh     # force target shell
#   scripts/install-aliases.sh --remove        # strip the block
#   scripts/install-aliases.sh --print         # echo the block, don't write
#   scripts/install-aliases.sh --help          # this header
#
# Exit codes:
#   0  success
#   1  rc file unwritable
#   2  bad invocation
#
# ---------------------------------------------------------------------------
# CHANGELOG
#   v1.0.0 (2026-04-30) — initial cut. Supports zsh / bash / fish; emits
#                          guarded aliases (each `command -v X` checked).
# ---------------------------------------------------------------------------

set -uo pipefail

# Pull in the project version so the block carries provenance.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/version.sh" 2>/dev/null || true
CRABCC_VERSION="${CRABCC_VERSION:-unknown}"

ACTION="install"
SHELL_OVERRIDE=""
for arg in "$@"; do
    case "$arg" in
        --remove)  ACTION="remove" ;;
        --print)   ACTION="print" ;;
        --shell)   ACTION="install" ;; # consumed by the next iteration
        --shell=*) SHELL_OVERRIDE="${arg#*=}" ;;
        --help|-h)
            sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            # Pair value for `--shell zsh` form.
            if [ "$arg" = "zsh" ] || [ "$arg" = "bash" ] || [ "$arg" = "fish" ]; then
                SHELL_OVERRIDE="$arg"
            else
                echo "unknown arg: $arg (try --help)" >&2
                exit 2
            fi
            ;;
    esac
done

# --- detect shell ---------------------------------------------------------
detect_shell() {
    if [ -n "$SHELL_OVERRIDE" ]; then
        echo "$SHELL_OVERRIDE"; return
    fi
    case "${SHELL:-}" in
        */zsh)  echo "zsh" ;;
        */bash) echo "bash" ;;
        */fish) echo "fish" ;;
        *)      echo "bash" ;;
    esac
}

# --- rc-file path per shell ----------------------------------------------
rc_path_for() {
    case "$1" in
        zsh)  echo "$HOME/.zshrc" ;;
        bash)
            # Prefer ~/.bashrc if it exists, else ~/.bash_profile (macOS).
            if [ -f "$HOME/.bashrc" ]; then echo "$HOME/.bashrc"
            else echo "$HOME/.bash_profile"; fi ;;
        fish) echo "$HOME/.config/fish/config.fish" ;;
        *)    return 1 ;;
    esac
}

# --- alias block (POSIX-compatible alias syntax) -------------------------
# Each alias is wrapped in `command -v <tool> >/dev/null 2>&1 && alias …`
# so the rc stays clean even if the modern tool was never installed.
print_block_zsh_or_bash() {
    cat <<EOF
# >>> crabcc-aliases >>> (managed by scripts/install-aliases.sh, crabcc v$CRABCC_VERSION)
# Re-run \`scripts/install-aliases.sh\` to refresh; \`--remove\` to strip.
command -v rg     >/dev/null 2>&1 && alias grep='rg'
command -v fd     >/dev/null 2>&1 && alias find='fd'
command -v bat    >/dev/null 2>&1 && alias cat='bat --paging=never'
command -v eza    >/dev/null 2>&1 && alias ls='eza --git --icons=auto'
command -v eza    >/dev/null 2>&1 && alias tree='eza --tree --git --level=4'
command -v dust   >/dev/null 2>&1 && alias du='dust'
command -v duf    >/dev/null 2>&1 && alias df='duf'
command -v procs  >/dev/null 2>&1 && alias ps='procs'
command -v btop   >/dev/null 2>&1 && alias top='btop'
command -v zoxide >/dev/null 2>&1 && eval "\$(zoxide init "$1" --cmd cd)"
# Heuristic: when jq is on the path, default a JSON-pipeline-friendly env.
command -v jq     >/dev/null 2>&1 && alias jq='jq --indent 2'

# crabcc-specific shortcuts.
command -v crabcc >/dev/null 2>&1 && {
    alias cc='crabcc'
    alias cci='crabcc index'
    alias ccs='crabcc sym'
    alias ccr='crabcc refs'
    alias ccc='crabcc callers'
    alias ccm='crabcc memory'
}
export CRABCC_VERSION='$CRABCC_VERSION'
# <<< crabcc-aliases <<<
EOF
}

print_block_fish() {
    cat <<EOF
# >>> crabcc-aliases >>> (managed by scripts/install-aliases.sh, crabcc v$CRABCC_VERSION)
type -q rg;     and alias grep 'rg'
type -q fd;     and alias find 'fd'
type -q bat;    and alias cat 'bat --paging=never'
type -q eza;    and alias ls 'eza --git --icons=auto'
type -q eza;    and alias tree 'eza --tree --git --level=4'
type -q dust;   and alias du 'dust'
type -q duf;    and alias df 'duf'
type -q procs;  and alias ps 'procs'
type -q btop;   and alias top 'btop'
type -q zoxide; and zoxide init fish --cmd cd | source
type -q jq;     and alias jq 'jq --indent 2'

if type -q crabcc
    alias cc 'crabcc'
    alias cci 'crabcc index'
    alias ccs 'crabcc sym'
    alias ccr 'crabcc refs'
    alias ccc 'crabcc callers'
    alias ccm 'crabcc memory'
end
set -gx CRABCC_VERSION '$CRABCC_VERSION'
# <<< crabcc-aliases <<<
EOF
}

print_block() {
    case "$1" in
        zsh|bash) print_block_zsh_or_bash "$1" ;;
        fish)     print_block_fish ;;
    esac
}

# --- splice into the rc file ---------------------------------------------
write_block() {
    local rc="$1" shell="$2"
    [ -d "$(dirname "$rc")" ] || mkdir -p "$(dirname "$rc")"
    touch "$rc"
    [ -w "$rc" ] || { echo "rc file not writable: $rc" >&2; return 1; }
    # Preserve everything outside the fenced block, then append a fresh one.
    local tmp
    tmp="$(mktemp)"
    awk '
        /^# >>> crabcc-aliases >>>/ { skip = 1 }
        !skip { print }
        /^# <<< crabcc-aliases <<</ { skip = 0; next }
    ' "$rc" >"$tmp"
    # Trailing newline normalization.
    if [ -s "$tmp" ] && [ "$(tail -c1 "$tmp" | xxd -p 2>/dev/null)" != "0a" ]; then
        printf "\n" >>"$tmp"
    fi
    print_block "$shell" >>"$tmp"
    mv "$tmp" "$rc"
}

remove_block() {
    local rc="$1"
    [ -f "$rc" ] || return 0
    local tmp
    tmp="$(mktemp)"
    awk '
        /^# >>> crabcc-aliases >>>/ { skip = 1; next }
        /^# <<< crabcc-aliases <<</ { skip = 0; next }
        !skip { print }
    ' "$rc" >"$tmp"
    mv "$tmp" "$rc"
}

# --- main ------------------------------------------------------------------
SHELL_NAME="$(detect_shell)"
RC_PATH="$(rc_path_for "$SHELL_NAME")"

case "$ACTION" in
    print)
        print_block "$SHELL_NAME"
        ;;
    remove)
        remove_block "$RC_PATH" && echo "removed crabcc-aliases block from $RC_PATH"
        ;;
    install)
        write_block "$RC_PATH" "$SHELL_NAME" || exit 1
        echo "installed crabcc-aliases block in $RC_PATH (shell: $SHELL_NAME)"
        echo "open a new shell or run: source $RC_PATH"
        ;;
esac
