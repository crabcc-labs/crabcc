#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/install-aliases.sh
#
# Install developer-friendly aliases for modern CLI tools into the user's
# shell rc file. Each alias is gated on the *modern* tool actually being
# present, so the rc never breaks if `bat` / `eza` / `rg` aren't yet
# installed.
#
# What this maps (always — minimal mode):
#   grep   → rg          (ripgrep — keeps regex, far faster on big trees)
#   find   → fd          (saner defaults, gitignore-aware)
#   cat    → bat         (syntax highlighting, retains pipe-friendliness)
#   ls     → eza         (color, git-aware, type icons)
#   du     → dust
#   df     → duf
#   ps     → procs
#   top    → btop
#   tree   → eza --tree
#   cd     → zoxide
#   tail   → lnav        (only when called as `tail -f`)
#
# Plus a small set of crabcc-specific aliases:
#   cc        → crabcc
#   cci       → crabcc index
#   ccs       → crabcc sym
#   ccr       → crabcc refs
#   ccc       → crabcc callers
#   ccm       → crabcc memory
#
# --aggressive adds (issue #81):
#   gr        → crabcc grep
#   sym       → crabcc sym
#   refs      → crabcc refs --files-only
#   callers   → crabcc callers --files-only
#   outline   → crabcc outline
#   fuzzy     → crabcc fuzzy
#   diff      → delta             (when delta is installed)
#
# Idempotent: writes a fenced `# >>> crabcc-aliases >>>` block; re-running
# replaces the block in place. `--remove` strips the block cleanly.
#
# Usage:
#   scripts/install-aliases.sh                    # detect shell, install minimal
#   scripts/install-aliases.sh --aggressive       # add crabcc verbs
#   scripts/install-aliases.sh --all-shells       # write to both .zshrc + .bashrc
#   scripts/install-aliases.sh --shell zsh        # force target shell
#   scripts/install-aliases.sh --remove           # strip the block
#   scripts/install-aliases.sh --print            # echo the block, don't write
#   scripts/install-aliases.sh --dry-run          # print rc path + block, no write
#   scripts/install-aliases.sh --install-tools    # brew/apt install missing modern tools
#   scripts/install-aliases.sh --help             # this header
#
# Flags compose: e.g. `--aggressive --all-shells --dry-run` previews a
# full install across zsh + bash without touching either rc file.
#
# Exit codes:
#   0  success
#   1  rc file unwritable
#   2  bad invocation
#
# ---------------------------------------------------------------------------
# CHANGELOG
#   v1.0.0 (2026-04-30) — initial cut. zsh / bash / fish; guarded aliases.
#   v1.1.0 (2026-04-30) — issue #81: --aggressive (crabcc verbs),
#                          --all-shells, --dry-run, --install-tools.
# ---------------------------------------------------------------------------

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/version.sh" 2>/dev/null || true
CRABCC_VERSION="${CRABCC_VERSION:-unknown}"

ACTION="install"
SHELL_OVERRIDE=""
AGGRESSIVE=0
ALL_SHELLS=0
DRY_RUN=0
INSTALL_TOOLS=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --remove)         ACTION="remove" ;;
        --print)          ACTION="print" ;;
        --dry-run)        DRY_RUN=1 ;;
        --aggressive)     AGGRESSIVE=1 ;;
        --all-shells)     ALL_SHELLS=1 ;;
        --install-tools)  INSTALL_TOOLS=1 ;;
        --shell)
            shift
            SHELL_OVERRIDE="${1:-}"
            ;;
        --shell=*)        SHELL_OVERRIDE="${1#*=}" ;;
        zsh|bash|fish)    SHELL_OVERRIDE="$1" ;;
        --help|-h)
            sed -n '2,55p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown arg: $1 (try --help)" >&2
            exit 2
            ;;
    esac
    shift
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
            if [ -f "$HOME/.bashrc" ]; then echo "$HOME/.bashrc"
            else echo "$HOME/.bash_profile"; fi ;;
        fish) echo "$HOME/.config/fish/config.fish" ;;
        *)    return 1 ;;
    esac
}

# --- alias block (POSIX-compatible alias syntax) -------------------------
print_block_zsh_or_bash() {
    local shell_name="$1"
    cat <<EOF
# >>> crabcc-aliases >>> (managed by scripts/install-aliases.sh, crabcc v$CRABCC_VERSION)
# Re-run \`scripts/install-aliases.sh\` to refresh; \`--remove\` to strip.
# Each alias is gated on \`command -v\` so missing tools never break the rc.
command -v rg     >/dev/null 2>&1 && alias grep='rg'
command -v fd     >/dev/null 2>&1 && alias find='fd'
command -v bat    >/dev/null 2>&1 && alias cat='bat --paging=never'
command -v eza    >/dev/null 2>&1 && alias ls='eza --git --icons=auto'
command -v eza    >/dev/null 2>&1 && alias tree='eza --tree --git --level=4'
command -v dust   >/dev/null 2>&1 && alias du='dust'
command -v duf    >/dev/null 2>&1 && alias df='duf'
command -v procs  >/dev/null 2>&1 && alias ps='procs'
command -v btop   >/dev/null 2>&1 && alias top='btop'
command -v zoxide >/dev/null 2>&1 && eval "\$(zoxide init "$shell_name" --cmd cd)"
command -v jq     >/dev/null 2>&1 && alias jq='jq --indent 2'

# crabcc / ccc shortcuts.
# Issue #74 — `ccc` is now a real binary (high-level combo CLI) installed
# next to crabcc. The previous `alias ccc='crabcc callers'` shadowed it,
# so it's been removed. Reach for `ccc` directly for the friendly surface;
# `crabcc` (or `cc`) for the low-level granular surface.
command -v crabcc >/dev/null 2>&1 && {
    alias cc='crabcc'
    alias cci='ccc index'
    alias ccs='ccc find'
    alias ccm='ccc memory'
}
EOF
    if [ "$AGGRESSIVE" = "1" ]; then
        cat <<'EOF'

# --- aggressive (--aggressive) — short verbs routed to ccc ---------------
# Issues #81, #74: route muscle memory ('where is X?', 'what calls Y?') to
# the high-level `ccc` binary first. Verbs that take an argument mid-line
# use shell functions (alias-with-args isn't portable); plain ones are
# aliases. All gated on the relevant binary being on PATH.
command -v ccc >/dev/null 2>&1 && {
    alias sym='ccc find'
    alias outline='crabcc outline'   # no ccc combo for outline yet
    alias fuzzy='ccc find --mode fuzzy'
    refs() { ccc find "$1" --mode references --files-only "${@:2}"; }
    callers() { ccc find "$1" --mode callers --files-only "${@:2}"; }
    gr() { ccc find "$1" --mode grep "${@:2}"; }
}
command -v delta >/dev/null 2>&1 && alias diff='delta'
EOF
    fi
    cat <<EOF
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
    alias cci 'ccc index'
    alias ccs 'ccc find'
    alias ccm 'ccc memory'
end
EOF
    if [ "$AGGRESSIVE" = "1" ]; then
        cat <<'EOF'

# --- aggressive (--aggressive) — short verbs routed to ccc ---------------
if type -q ccc
    alias sym 'ccc find'
    alias outline 'crabcc outline'   # no ccc combo for outline yet
    alias fuzzy 'ccc find --mode fuzzy'
    function refs;     ccc find $argv[1] --mode references --files-only $argv[2..];   end
    function callers;  ccc find $argv[1] --mode callers --files-only $argv[2..];      end
    function gr;       ccc find $argv[1] --mode grep $argv[2..];                       end
end
type -q delta; and alias diff 'delta'
EOF
    fi
    cat <<EOF
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
    local tmp
    tmp="$(mktemp)"
    awk '
        /^# >>> crabcc-aliases >>>/ { skip = 1 }
        !skip { print }
        /^# <<< crabcc-aliases <<</ { skip = 0; next }
    ' "$rc" >"$tmp"
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

# --- modern-tool installer (--install-tools) -----------------------------
# Idempotent: skips tools already on PATH. Uses brew on macOS, apt on Linux.
install_modern_tools() {
    local tools="ripgrep fd bat eza dust duf procs btop zoxide jq git-delta"
    local missing=()
    for t in rg fd bat eza dust duf procs btop zoxide jq delta; do
        command -v "$t" >/dev/null 2>&1 || missing+=("$t")
    done
    if [ "${#missing[@]}" -eq 0 ]; then
        echo "all modern tools present — nothing to install"
        return 0
    fi
    echo "missing: ${missing[*]}"
    if command -v brew >/dev/null 2>&1; then
        # shellcheck disable=SC2086
        echo "+ brew install $tools"
        [ "$DRY_RUN" = "1" ] && return 0
        # shellcheck disable=SC2086
        brew install $tools
    elif command -v apt-get >/dev/null 2>&1; then
        # apt naming differs slightly: bat → batcat, fd → fdfind on Debian.
        local apt_pkgs="ripgrep fd-find bat eza dust duf procs btop zoxide jq git-delta"
        echo "+ sudo apt-get install -y $apt_pkgs"
        [ "$DRY_RUN" = "1" ] && return 0
        sudo apt-get update && sudo apt-get install -y $apt_pkgs
    else
        echo "no supported package manager (brew/apt) found; install manually:" >&2
        echo "  ${missing[*]}" >&2
        return 1
    fi
}

# --- main ------------------------------------------------------------------
SHELLS_TO_INSTALL=()
if [ "$ALL_SHELLS" = "1" ]; then
    SHELLS_TO_INSTALL=(zsh bash)
else
    SHELLS_TO_INSTALL=("$(detect_shell)")
fi

if [ "$INSTALL_TOOLS" = "1" ]; then
    install_modern_tools || true
fi

case "$ACTION" in
    print)
        for sh in "${SHELLS_TO_INSTALL[@]}"; do
            echo "# --- $sh ---"
            print_block "$sh"
        done
        ;;
    remove)
        for sh in "${SHELLS_TO_INSTALL[@]}"; do
            rc="$(rc_path_for "$sh")"
            if [ "$DRY_RUN" = "1" ]; then
                echo "(dry-run) would remove crabcc-aliases block from $rc"
            else
                remove_block "$rc" && echo "removed crabcc-aliases block from $rc"
            fi
        done
        ;;
    install)
        for sh in "${SHELLS_TO_INSTALL[@]}"; do
            rc="$(rc_path_for "$sh")"
            if [ "$DRY_RUN" = "1" ]; then
                echo "(dry-run) target rc: $rc (shell: $sh, aggressive=$AGGRESSIVE)"
                print_block "$sh"
            else
                write_block "$rc" "$sh" || exit 1
                echo "installed crabcc-aliases block in $rc (shell: $sh, aggressive=$AGGRESSIVE)"
            fi
        done
        if [ "$DRY_RUN" != "1" ]; then
            echo "open a new shell or source the rc file(s) above"
        fi
        ;;
esac
