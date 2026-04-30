#!/usr/bin/env bash
# install-container-completions.sh — install Apple `container` shell
# completions + a friendly `c` alias. Idempotent.
#
# What this does:
#   1. Detects the active shell (zsh / bash / fish; defaults to zsh on macOS).
#   2. Generates completions via `container --generate-completion-script <shell>`
#      and drops them into a directory the shell already auto-loads:
#        zsh + oh-my-zsh : ~/.oh-my-zsh/completions/_container
#        zsh             : ~/.zsh/completion/_container  (+ fpath wire-up)
#        bash + brew     : $(brew --prefix)/etc/bash_completion.d/container
#        bash            : ~/.bash_completions/container (+ source line)
#        fish            : ~/.config/fish/completions/container.fish
#   3. Adds a fenced shell-rc block defining a `c` alias for `container`
#      (matches the existing crabcc `cc → crabcc` style).
#   4. Sources the new completion in the current shell when sourced
#      with `source` instead of `bash …`. When invoked as a subprocess,
#      prints the source command + a clear "RELOAD CLAUDE CODE" warning.
#
# Usage:
#   bash scripts/install-container-completions.sh
#   bash scripts/install-container-completions.sh --shell zsh
#   bash scripts/install-container-completions.sh --remove
#   bash scripts/install-container-completions.sh --print

set -uo pipefail

SHELL_OVERRIDE=""
ACTION="install"
for arg in "$@"; do
    case "$arg" in
        --shell)         shift; SHELL_OVERRIDE="${1:-}" ;;
        --shell=*)       SHELL_OVERRIDE="${arg#--shell=}" ;;
        --remove|-r)     ACTION="remove" ;;
        --print|-p)      ACTION="print" ;;
        --help|-h)       sed -n '1,30p' "${BASH_SOURCE[0]:-$0}"; exit 0 ;;
    esac
done

if [[ -t 1 ]]; then BOLD='\033[1m'; YEL='\033[33m'; CYN='\033[36m'; OFF='\033[0m'; else BOLD=''; YEL=''; CYN=''; OFF=''; fi
info()  { printf "%b%s%b\n" "$CYN" "$*" "$OFF"; }
warn()  { printf "%b!! %s%b\n" "$YEL" "$*" "$OFF" >&2; }
done_() { printf "%b✓ %s%b\n" "$BOLD" "$*" "$OFF"; }

if ! command -v container >/dev/null 2>&1; then
    warn "Apple 'container' is not on PATH."
    warn "Install with: brew install apple/container/container"
    warn "  or download:  https://github.com/apple/container/releases"
    exit 1
fi

# ---- detect shell --------------------------------------------------------

detect_shell() {
    if [[ -n "$SHELL_OVERRIDE" ]]; then
        echo "$SHELL_OVERRIDE"; return
    fi
    case "$(basename "${SHELL:-}")" in
        zsh)  echo "zsh" ;;
        bash) echo "bash" ;;
        fish) echo "fish" ;;
        *)    echo "zsh" ;;  # macOS default
    esac
}
SH="$(detect_shell)"

# ---- alias block (fenced; same shape as install-aliases.sh) --------------

ALIAS_BLOCK_BEGIN='# >>> crabcc-container-aliases >>>'
ALIAS_BLOCK_END='# <<< crabcc-container-aliases <<<'
read -r -d '' ALIAS_BODY <<'EOF' || true
# Apple `container` (https://github.com/apple/container)
# Installed by crabcc/scripts/install-container-completions.sh
alias c='container'
alias cps='container ls --format table'
alias clog='container logs -f'
alias cstats='container stats --no-stream'
EOF

rc_path_for() {
    case "$1" in
        zsh)  echo "$HOME/.zshrc" ;;
        bash) echo "$HOME/.bashrc" ;;
        fish) echo "$HOME/.config/fish/config.fish" ;;
    esac
}

write_alias_block() {
    local rc; rc="$(rc_path_for "$SH")"
    [[ -z "$rc" ]] && return 1
    [[ -f "$rc" ]] || touch "$rc"
    if grep -qF "$ALIAS_BLOCK_BEGIN" "$rc"; then
        # Replace existing block in place.
        local tmp; tmp="$(mktemp)"
        awk -v b="$ALIAS_BLOCK_BEGIN" -v e="$ALIAS_BLOCK_END" '
            $0 == b { skip = 1; next }
            $0 == e { skip = 0; next }
            !skip { print }
        ' "$rc" > "$tmp"
        mv "$tmp" "$rc"
    fi
    {
        printf '\n%s\n' "$ALIAS_BLOCK_BEGIN"
        printf '%s\n'   "$ALIAS_BODY"
        printf '%s\n'   "$ALIAS_BLOCK_END"
    } >> "$rc"
    info "alias block written to $rc"
}

remove_alias_block() {
    local rc; rc="$(rc_path_for "$SH")"
    [[ -f "$rc" ]] || return 0
    if grep -qF "$ALIAS_BLOCK_BEGIN" "$rc"; then
        local tmp; tmp="$(mktemp)"
        awk -v b="$ALIAS_BLOCK_BEGIN" -v e="$ALIAS_BLOCK_END" '
            $0 == b { skip = 1; next }
            $0 == e { skip = 0; next }
            !skip { print }
        ' "$rc" > "$tmp"
        mv "$tmp" "$rc"
        info "alias block removed from $rc"
    fi
}

# ---- completion target paths --------------------------------------------

zsh_target() {
    if [[ -d "$HOME/.oh-my-zsh" ]]; then
        echo "$HOME/.oh-my-zsh/completions/_container"
    else
        echo "$HOME/.zsh/completion/_container"
    fi
}
bash_target() {
    if command -v brew >/dev/null 2>&1; then
        echo "$(brew --prefix)/etc/bash_completion.d/container"
    else
        echo "$HOME/.bash_completions/container"
    fi
}
fish_target() {
    echo "$HOME/.config/fish/completions/container.fish"
}

target_for() {
    case "$SH" in
        zsh)  zsh_target ;;
        bash) bash_target ;;
        fish) fish_target ;;
    esac
}

# ---- actions -------------------------------------------------------------

dest="$(target_for)"

if [[ "$ACTION" == "print" ]]; then
    info "shell: $SH"
    info "target: $dest"
    info "alias rc: $(rc_path_for "$SH")"
    info "preview (first 8 lines):"
    container --generate-completion-script "$SH" | head -8
    exit 0
fi

if [[ "$ACTION" == "remove" ]]; then
    if [[ -f "$dest" ]]; then
        rm -f "$dest"
        info "removed $dest"
    fi
    remove_alias_block
    done_ "removed (re-open the shell)"
    exit 0
fi

# install
mkdir -p "$(dirname "$dest")"
container --generate-completion-script "$SH" > "$dest"
chmod 0644 "$dest"
done_ "completions written → $dest"

# Bash without bash-completion: ensure ~/.bashrc sources our file.
if [[ "$SH" == "bash" ]] && [[ "$dest" == "$HOME/.bash_completions/container" ]]; then
    rc="$HOME/.bashrc"
    line="source $dest"
    if ! grep -qxF "$line" "$rc" 2>/dev/null; then
        printf '\n# crabcc/container completions\n%s\n' "$line" >> "$rc"
        info "added source line to $rc"
    fi
fi

# zsh without oh-my-zsh: ensure ~/.zshrc has fpath + autoload setup.
if [[ "$SH" == "zsh" ]] && [[ "$dest" == "$HOME/.zsh/completion/_container" ]]; then
    rc="$HOME/.zshrc"
    if ! grep -qxF 'fpath=(~/.zsh/completion $fpath)' "$rc" 2>/dev/null; then
        {
            printf '\n# crabcc/container completions\n'
            printf 'fpath=(~/.zsh/completion $fpath)\n'
            printf 'autoload -U compinit\ncompinit\n'
        } >> "$rc"
        info "added fpath + compinit to $rc"
    fi
fi

write_alias_block

# ---- reload guidance -----------------------------------------------------

cat <<EOF

  ${BOLD}Next:${OFF}
    1. Reload your shell:
         source $(rc_path_for "$SH")
       Or close + reopen the terminal.
    2. ${YEL}WARN — RELOAD CLAUDE CODE.${OFF}
       Running Claude sessions inherited the OLD env (no completion path,
       no aliases). New shell hooks won't apply until you fully restart
       the Claude Code app — quit + reopen, do not just reload the window.
       Tip: \`/exit\` then re-launch from the dock so PATH + functions
       re-read from the rc you just touched.

EOF

done_ "container completions + 'c' alias installed"
