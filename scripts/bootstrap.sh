#!/usr/bin/env bash
# bootstrap.sh — one-shot crabcc setup for a fresh macOS / Linux machine.
#
# Designed to be `curl | bash`-able AND interactively friendly.
# Idempotent: same script for fresh install, update, and upgrade.
# Stages the repo under ~/workspace/bin/crabcc (configurable via $CRABCC_HOME).
#
#   curl -fsSL https://raw.githubusercontent.com/peterlodri-sec/crabcc/main/scripts/bootstrap.sh | bash
#
# What it does (in order, each step idempotent):
#   1. Preflight — verify git, curl, cargo (rustup if missing).
#   2. Clone or update the repo at ${CRABCC_HOME:-$HOME/workspace/bin/crabcc}.
#   3. cargo install --path crates/crabcc-cli  (writes crabcc + ccc to ~/.cargo/bin).
#   4. macOS only: ad-hoc codesign the binaries (Sequoia provenance fix).
#   5. Run scripts/install-aliases.sh --all-shells (idempotent).
#   6. Symlink skill/ + commands/ into ~/.claude/.
#   7. Docker + Ollama stack — on by default; pass --no-docker to skip.
#   8. Optional macOS LaunchAgent — only if --with-launchd is passed (macOS).
#   9. Optional macOS .app build — only if --with-macos-app is passed (macOS).
#  10. Run `scripts/doctor.sh --quiet` and a built-in --verify check.
#
# Modes:
#   (TTY, no flags)     interactive menu
#   (piped, no flags)   full install (preserves curl|bash UX)
#
# Flags:
#   --menu              force the menu (override TTY detection)
#   --verify            run verification only — no install
#   --show-keys         show API keys + secrets created by previous installs
#   --cli-only          install crabcc + ccc binaries only
#   --macos-app-only    build + open Crabcc.dmg only (macOS)
#   --telegram-only     bootstrap Telegram bot scaffolding only
#   --ollama-only       Docker + Ollama stack + key mint only
#   --with-docker       no-op (Docker + Ollama is now default-on)
#   --no-docker         skip Docker + Ollama stack in full install
#   --with-launchd      include LaunchAgents in full install (macOS)
#   --with-macos-app    include DMG build in full install (macOS)
#   --branch <name>     clone a non-main branch (useful for testing)
#   --check-only        preflight only — no writes
#   --no-aliases        skip shell alias install
#   --verbose, -v       extra timestamped detail in logs
#   --help, -h          this header
#
# Exit codes:
#   0  success
#   1  install failed
#   2  bad invocation
#   3  verification failed

set -uo pipefail

# === defaults ============================================================

readonly _BS_VERSION="2.0"
readonly _BS_HOME_DEFAULT="$HOME/workspace/bin/crabcc"
readonly _BS_REPO_DEFAULT="https://github.com/peterlodri-sec/crabcc.git"
readonly _BS_BRANCH_DEFAULT="main"

# === helpers (always defined; safe to source) ============================

# shellcheck disable=SC2034  # color codes are referenced indirectly via printf
c_blue='\033[1;34m'; c_grn='\033[1;32m'; c_yel='\033[1;33m'; c_red='\033[1;31m'
c_dim='\033[2m';     c_off='\033[0m'

log()  { printf "%b▶%b %s\n" "$c_blue" "$c_off" "$*"; }
ok()   { printf "%b✓%b %s\n" "$c_grn"  "$c_off" "$*"; }
warn() { printf "%b!%b %s\n" "$c_yel"  "$c_off" "$*" >&2; }
die()  { printf "%b✗%b %s\n" "$c_red"  "$c_off" "$*" >&2; exit 1; }

# Verbose log: only fires when VERBOSE=1, prepends timestamp.
vlog() {
    [[ "${VERBOSE:-0}" -eq 1 ]] || return 0
    printf "%b    [%s] %s%b\n" "$c_dim" "$(date +%H:%M:%S)" "$*" "$c_off"
}

have() { command -v "$1" >/dev/null 2>&1; }

# Mask a secret to first-4…last-4 form. Strings ≤8 chars become "<N chars>".
mask() {
    local s="${1:-}"
    if [[ -z "$s" ]]; then printf '<empty>'; return; fi
    local len=${#s}
    if (( len <= 8 )); then printf '<%d chars>' "$len"; return; fi
    printf '%s…%s' "${s:0:4}" "${s: -4}"
}

# Read KEY=value from a .env-style file; trims surrounding quotes.
read_env_var() {
    local file="$1" key="$2"
    [[ -f "$file" ]] || return 1
    local raw
    raw=$(grep -E "^[[:space:]]*${key}=" "$file" 2>/dev/null | tail -1) || return 1
    [[ -n "$raw" ]] || return 1
    raw="${raw#*=}"
    raw="${raw%\"}"; raw="${raw#\"}"
    raw="${raw%\'}"; raw="${raw#\'}"
    printf '%s' "$raw"
}

# Portable file-mode (octal) for a path. macOS uses BSD stat; Linux GNU stat.
file_mode() {
    stat -f '%Lp' "$1" 2>/dev/null || stat -c '%a' "$1" 2>/dev/null || printf '?'
}

# Portable mtime (epoch seconds).
file_mtime() {
    stat -f '%m' "$1" 2>/dev/null || stat -c '%Y' "$1" 2>/dev/null || printf '0'
}

# === step functions ======================================================

step_preflight() {
    log "preflight: checking required tools"
    have git  || die "git is required (xcode-select --install on macOS, apt install git on Debian)"
    have curl || die "curl is required"

    if ! have cargo; then
        if [[ "${CHECK_ONLY:-0}" -eq 1 ]]; then
            warn "cargo missing (would install rustup)"
        else
            log "installing rustup (cargo not found)"
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
                | sh -s -- -y --default-toolchain stable
            # shellcheck disable=SC1091
            . "$HOME/.cargo/env"
        fi
    fi
    export PATH="$HOME/.cargo/bin:$PATH"

    if [[ "${IS_MAC:-0}" -eq 1 ]]; then
        have codesign || warn "codesign missing — install Xcode Command Line Tools"
    fi

    ok "preflight passed"
    vlog "cargo: $(command -v cargo 2>/dev/null || echo none)"
    vlog "git:   $(git --version 2>/dev/null || echo none)"
    vlog "OS:    $(uname -srm)"
}

step_clone_or_update() {
    mkdir -p "$(dirname "$CRABCC_HOME")"
    if [[ -d "$CRABCC_HOME/.git" ]]; then
        log "updating $CRABCC_HOME (branch: $BRANCH)"
        git -C "$CRABCC_HOME" fetch --tags origin
        git -C "$CRABCC_HOME" checkout "$BRANCH"
        if [[ -z "$(git -C "$CRABCC_HOME" status --porcelain)" ]]; then
            git -C "$CRABCC_HOME" pull --rebase --autostash origin "$BRANCH"
            ok "updated to $(git -C "$CRABCC_HOME" rev-parse --short HEAD)"
        else
            warn "$CRABCC_HOME has uncommitted changes — skipping pull"
        fi
    else
        log "cloning $REPO_URL (branch: $BRANCH) → $CRABCC_HOME"
        git clone --branch "$BRANCH" --depth 50 "$REPO_URL" "$CRABCC_HOME"
        ok "cloned $(git -C "$CRABCC_HOME" rev-parse --short HEAD)"
    fi
    cd "$CRABCC_HOME" || die "cannot cd into $CRABCC_HOME"
    vlog "pwd: $PWD"
}

step_cli_install() {
    log "cargo install --path crates/crabcc-cli (~30-60s on first run)"
    if [[ "${VERBOSE:-0}" -eq 1 ]]; then
        cargo install --path crates/crabcc-cli
    else
        cargo install --path crates/crabcc-cli --quiet
    fi
    ok "installed crabcc + ccc to $HOME/.cargo/bin"
    vlog "crabcc: $(command -v crabcc 2>/dev/null || echo none)"
    vlog "ccc:    $(command -v ccc 2>/dev/null || echo none)"
}

step_codesign_and_smoke() {
    if [[ "${IS_MAC:-0}" -eq 1 ]] && have codesign; then
        for b in crabcc ccc; do
            local p="$HOME/.cargo/bin/$b"
            [[ -x "$p" ]] || continue
            if /usr/bin/codesign --force --sign - "$p" 2>/dev/null; then
                vlog "codesigned $p (ad-hoc)"
            else
                warn "codesign failed for $p"
            fi
        done
    fi
    if "$HOME/.cargo/bin/crabcc" --version >/dev/null 2>&1; then
        ok "crabcc runs ($(crabcc --version))"
    else
        die "crabcc failed to run after install"
    fi
}

step_aliases() {
    log "installing shell aliases"
    if bash "$CRABCC_HOME/scripts/install-aliases.sh" --all-shells; then
        ok "aliases installed (zsh + bash)"
    else
        warn "alias install returned non-zero"
    fi
}

step_skills_commands() {
    # Single source of truth: delegate to `crabcc install-claude` so
    # this script doesn't drift from `crabcc install-claude`'s
    # behaviour (notably the RTK detection added in #478 — the prompt
    # offers `cargo install rtk` if Rust Token Killer isn't on PATH).
    # `--yes` skips the per-symlink confirmations; symlinks point at
    # `$CRABCC_HOME/skill/...` which is stable for bootstrap.sh's
    # install model (the clone lives in `~/workspace/bin/crabcc`,
    # not a tempdir).
    log "running crabcc install-claude (symlinks skill + commands; offers RTK)"
    # Don't suppress stdout/stderr — the user wants to see the RTK
    # detection result and any cargo-install progress. Capture only
    # exit status so a non-zero failure surfaces as a warn rather
    # than aborting the whole bootstrap.
    if "$HOME/.cargo/bin/crabcc" install-claude --yes; then
        ok "claude integration wired (skill + commands + RTK check)"
    else
        warn "crabcc install-claude returned non-zero — continuing"
        warn "    re-run manually with \`crabcc install-claude\` to inspect"
    fi
}

step_macos_app() {
    if [[ "${IS_MAC:-0}" -ne 1 ]]; then
        warn "macOS app step skipped (not Darwin)"
        return 0
    fi
    log "building Crabcc.dmg (rebuilds release binaries)"
    if bash "$CRABCC_HOME/scripts/build-dmg.sh"; then
        ok "DMG built — opening dist/"
        open "$CRABCC_HOME/dist/" 2>/dev/null || vlog "skipped 'open dist/' (no DISPLAY?)"
    else
        warn "DMG build failed"
    fi
}

step_telegram() {
    log "Telegram bot setup"
    local env_file="$CRABCC_HOME/apps/crabcc-telegram/.env"
    local example="${env_file}.example"
    if [[ ! -f "$env_file" ]]; then
        if [[ -f "$example" ]]; then
            cp "$example" "$env_file"
            chmod 600 "$env_file"
            warn "created $env_file from .example — edit it with your bot token (BotFather)"
        else
            warn "no $env_file or .example template found"
        fi
    else
        ok ".env already exists at $env_file"
    fi
    log "build:  cargo build -p crabcc-telegram"
    log "run:    task telegram-bot   (or: cargo run -p crabcc-telegram)"
}

step_ollama_stack() {
    log "Ollama stack setup"
    if ! have docker; then
        if [[ "${IS_MAC:-0}" -eq 1 ]]; then
            if have brew; then
                brew install --cask docker
                ok "Docker Desktop installed via Homebrew"
                warn "launch Docker.app once to grant privacy permissions"
            else
                warn "Homebrew not found — install Docker manually from https://docker.com"
            fi
        else
            warn "Linux: install Docker via your package manager (apt/dnf/pacman)"
        fi
    else
        vlog "docker: $(docker --version)"
    fi

    local compose="$CRABCC_HOME/install/ollama-stack/docker-compose.yml"
    if [[ -f "$compose" ]] && have docker; then
        log "starting Ollama stack (docker compose up -d --wait)"
        if (cd "$(dirname "$compose")" && docker compose up -d --wait); then
            ok "Ollama stack running"
            local keys_script="$CRABCC_HOME/install/ollama-stack/init-keys.sh"
            if [[ -x "$keys_script" ]] && [[ ! -f "$HOME/.crabcc.local.api-key" ]]; then
                log "minting API key via init-keys.sh"
                if bash "$keys_script"; then
                    ok "API key written to ~/.crabcc.local.api-key"
                else
                    warn "init-keys.sh returned non-zero"
                fi
            fi
        else
            warn "docker compose failed"
        fi
    else
        warn "no $compose — skipping stack startup"
    fi
}

step_launchd() {
    if [[ "${IS_MAC:-0}" -ne 1 ]]; then
        warn "launchd step skipped (not Darwin)"
        return 0
    fi
    log "registering crabcc-agentd LaunchAgent"
    if bash "$CRABCC_HOME/scripts/install-macos-helpers.sh"; then
        ok "agentd registered"
    else
        warn "agentd registration failed"
    fi
}

step_doctor() {
    log "running doctor"
    bash "$CRABCC_HOME/scripts/doctor.sh" --quiet \
        || warn "doctor reported issues — see output above"
}

# === do_verify: post-install correctness check ===========================

do_verify() {
    local home="${BS_TEST_HOME:-$HOME}"
    local crabcc_home="${CRABCC_HOME:-$home/workspace/bin/crabcc}"
    local fails=0 checks=0

    _v_check() {
        local label="$1"; shift
        checks=$((checks+1))
        if "$@" >/dev/null 2>&1; then
            ok "verify: $label"
        else
            printf "%b✗%b verify: %s — failed\n" "$c_red" "$c_off" "$label" >&2
            fails=$((fails+1))
        fi
    }

    log "verifying install (HOME=$home, CRABCC_HOME=$crabcc_home)"

    # In test mode (BS_TEST_HOME set), prefer absolute paths under the mock
    # over `command -v` which would find the real install.
    if [[ -n "${BS_TEST_HOME:-}" ]]; then
        _v_check "crabcc binary present"   test -x "$home/.cargo/bin/crabcc"
        _v_check "ccc binary present"      test -x "$home/.cargo/bin/ccc"
    else
        _v_check "crabcc on PATH"          have crabcc
        _v_check "ccc on PATH"             have ccc
    fi

    _v_check "skills symlinked"            test -d "$home/.claude/skills/crabcc"
    _v_check "crabcc-init linked"          bash -c "test -L \"$home/.claude/commands/crabcc-init.md\" -o -f \"$home/.claude/commands/crabcc-init.md\""
    _v_check "crabcc repo present"         test -d "$crabcc_home/.git"

    if [[ "${IS_MAC:-0}" -eq 1 ]]; then
        _v_check "Crabcc.app present (any of /Applications, $crabcc_home/installer, $crabcc_home/build/dmg/dmg-stage)" \
            bash -c "test -d /Applications/Crabcc.app -o -d \"$crabcc_home/installer/Crabcc.app\" -o -d \"$crabcc_home/build/dmg/dmg-stage/Crabcc.app\""
    fi

    # Optional surfaces (informational, don't fail)
    if [[ -f "$home/.crabcc.local.api-key" ]]; then
        ok "verify: ollama-stack key present (mode $(file_mode "$home/.crabcc.local.api-key"))"
    else
        vlog "ollama-stack key not present (run --ollama-only to mint)"
    fi

    echo
    if (( fails == 0 )); then
        ok "verify passed: $checks/$checks checks"
        return 0
    fi
    printf "%b✗%b verify failed: %d/%d checks failed\n" "$c_red" "$c_off" "$fails" "$checks" >&2
    return 3
}

# === do_show_keys: surface API keys + secrets ============================

do_show_keys() {
    local home="${BS_TEST_HOME:-$HOME}"
    local crabcc_home="${CRABCC_HOME:-$home/workspace/bin/crabcc}"

    log "API keys + secrets created by crabcc installs"
    echo

    # 1. Ollama-stack key
    local ollama_key="$home/.crabcc.local.api-key"
    if [[ -f "$ollama_key" ]]; then
        local mode age val now
        mode=$(file_mode "$ollama_key")
        now=$(date +%s)
        age=$(( (now - $(file_mtime "$ollama_key")) / 86400 ))
        val=$(<"$ollama_key")
        printf "  %b●%b Ollama-stack key  %s  mode=%s  age=%dd  val=%s\n" \
            "$c_grn" "$c_off" "$ollama_key" "$mode" "$age" "$(mask "$val")"
    else
        printf "  %b○ Ollama-stack key  not present (run with --ollama-only)%b\n" "$c_dim" "$c_off"
    fi

    # 2. Telegram bot token (apps/crabcc-telegram/.env preferred, root .env fallback)
    local tg_env="$crabcc_home/apps/crabcc-telegram/.env"
    local tg_root="$crabcc_home/.env"
    local tg_tok=""
    local tg_src=""
    if [[ -f "$tg_env" ]]; then
        tg_tok=$(read_env_var "$tg_env" TELEGRAM_BOT_TOKEN 2>/dev/null || true)
        tg_src="$tg_env"
    fi
    if [[ -z "$tg_tok" || "$tg_tok" == \<*\> ]] && [[ -f "$tg_root" ]]; then
        tg_tok=$(read_env_var "$tg_root" TELEGRAM_BOT_TOKEN 2>/dev/null || true)
        tg_src="$tg_root"
    fi
    if [[ -n "$tg_tok" && "$tg_tok" != \<*\> ]]; then
        printf "  %b●%b Telegram token   %s  val=%s\n" "$c_grn" "$c_off" "$tg_src" "$(mask "$tg_tok")"
    else
        printf "  %b○ Telegram token   not configured (edit %s)%b\n" "$c_dim" "$tg_env" "$c_off"
    fi

    # 3. LiteLLM master key
    local lite_env="$crabcc_home/install/ollama-stack/.env"
    local lite_key=""
    [[ -f "$lite_env" ]] && lite_key=$(read_env_var "$lite_env" LITELLM_MASTER_KEY 2>/dev/null || true)
    if [[ -n "$lite_key" ]]; then
        printf "  %b●%b LiteLLM master   %s  val=%s\n" "$c_grn" "$c_off" "$lite_env" "$(mask "$lite_key")"
    else
        printf "  %b○ LiteLLM master   not present at %s%b\n" "$c_dim" "$lite_env" "$c_off"
    fi

    # 4. GitHub PAT in ~/.claude/settings.local.json env
    local cc_settings="$home/.claude/settings.local.json"
    if [[ -f "$cc_settings" ]] && have jq; then
        local gh_pat
        gh_pat=$(jq -r '.env.GITHUB_PERSONAL_ACCESS_TOKEN // empty' "$cc_settings" 2>/dev/null || true)
        if [[ -n "$gh_pat" ]]; then
            printf "  %b●%b GitHub PAT       %s (env block, mode=%s)  val=%s\n" \
                "$c_grn" "$c_off" "$cc_settings" "$(file_mode "$cc_settings")" "$(mask "$gh_pat")"
        else
            printf "  %b○ GitHub PAT       not in %s env block%b\n" "$c_dim" "$cc_settings" "$c_off"
        fi
    else
        printf "  %b○ GitHub PAT       no %s (or jq missing)%b\n" "$c_dim" "$cc_settings" "$c_off"
    fi

    echo
    log "to reveal a value in full: cat <path-shown-above>"
}

# === do_menu: interactive picker =========================================

do_menu() {
    cat <<'MENU'

  ╭───────────────────────────────────────────────╮
  │  crabcc setup — what would you like to do?    │
  ╰───────────────────────────────────────────────╯

    1) Install everything             (recommended for first run)
    2) CLI only                       (cargo install crabcc + ccc)
    3) macOS .app only                (build + open Crabcc.dmg)
    4) Ollama stack only              (Docker + LiteLLM + key mint)
    5) Telegram bot only              (init .env, build hint)
    6) Show API keys created
    7) Verify current install
    8) Quit

MENU
    local choice
    while :; do
        printf "  ↳ choice [1-8]: "
        if ! IFS= read -r choice </dev/tty 2>/dev/null; then
            warn "no TTY — aborting menu (use a non --menu flag instead)"
            exit 1
        fi
        case "$choice" in
            1) WITH_DOCKER=1; WITH_LAUNCHD=1; WITH_MACOS_APP=1; return 0 ;;
            2) CLI_ONLY=1;       return 0 ;;
            3) MACOS_APP_ONLY=1; return 0 ;;
            4) OLLAMA_ONLY=1;    return 0 ;;
            5) TELEGRAM_ONLY=1;  return 0 ;;
            6) do_show_keys; exit 0 ;;
            7) do_verify;    exit $? ;;
            8) ok "bye"; exit 0 ;;
            *) printf "  invalid choice — try again\n" ;;
        esac
    done
}

# === main ================================================================

main() {
    # Defaults (overridable via env)
    CRABCC_HOME="${CRABCC_HOME:-$_BS_HOME_DEFAULT}"
    REPO_URL="${CRABCC_REPO_URL:-$_BS_REPO_DEFAULT}"
    BRANCH="$_BS_BRANCH_DEFAULT"

    # Docker + Ollama stack is on by default — the canonical
    # "I want a working agent backend" experience needs the LiteLLM
    # proxy + Ollama running. `--no-docker` opts out for headless /
    # CI / "just give me the binary" cases. `--with-docker` is kept
    # as a no-op for backwards compat with older invocations.
    WITH_DOCKER=1
    WITH_LAUNCHD=0
    WITH_MACOS_APP=0
    NO_ALIASES=0
    CHECK_ONLY=0

    MENU=0
    VERIFY_ONLY=0
    SHOW_KEYS=0
    CLI_ONLY=0
    MACOS_APP_ONLY=0
    TELEGRAM_ONLY=0
    OLLAMA_ONLY=0
    VERBOSE=0

    OS="$(uname -s)"
    IS_MAC=0; [[ "$OS" == "Darwin" ]] && IS_MAC=1

    # Parse args
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --menu)            MENU=1 ;;
            --verify)          VERIFY_ONLY=1 ;;
            --show-keys)       SHOW_KEYS=1 ;;
            --cli-only)        CLI_ONLY=1 ;;
            --macos-app-only)  MACOS_APP_ONLY=1 ;;
            --telegram-only)   TELEGRAM_ONLY=1 ;;
            --ollama-only)     OLLAMA_ONLY=1 ;;
            --with-docker)     WITH_DOCKER=1 ;;       # legacy / no-op (default-on)
            --no-docker)       WITH_DOCKER=0 ;;
            --with-launchd)    WITH_LAUNCHD=1 ;;
            --with-macos-app)  WITH_MACOS_APP=1 ;;
            --branch)          BRANCH="$2"; shift ;;
            --check-only)      CHECK_ONLY=1 ;;
            --no-aliases)      NO_ALIASES=1 ;;
            --verbose|-v)      VERBOSE=1 ;;
            --help|-h)         sed -n '1,55p' "${BASH_SOURCE[0]:-$0}"; return 0 ;;
            *)                 printf 'unknown flag: %s\n' "$1" >&2; return 2 ;;
        esac
        shift
    done

    # Read-only modes (no install side effects)
    if [[ "$VERIFY_ONLY" -eq 1 ]]; then do_verify;    return $?; fi
    if [[ "$SHOW_KEYS"  -eq 1 ]]; then do_show_keys;  return 0;  fi

    # Auto-menu when interactive TTY + no install flags chosen
    local install_flags_count=$(( WITH_DOCKER + WITH_LAUNCHD + WITH_MACOS_APP \
                                + CLI_ONLY + MACOS_APP_ONLY + TELEGRAM_ONLY + OLLAMA_ONLY \
                                + CHECK_ONLY ))
    if [[ "$MENU" -eq 1 ]] \
       || ([[ -t 0 ]] && [[ -t 1 ]] && (( install_flags_count == 0 )) \
           && [[ "${BS_NONINTERACTIVE:-0}" -ne 1 ]]); then
        do_menu
    fi

    # Subset shortcuts (set by menu or by flag)
    if [[ "$CLI_ONLY" -eq 1 ]]; then
        step_preflight; step_clone_or_update; step_cli_install; step_codesign_and_smoke
        ok "CLI install complete"
        return 0
    fi
    if [[ "$MACOS_APP_ONLY" -eq 1 ]]; then
        step_preflight; step_clone_or_update; step_macos_app
        return 0
    fi
    if [[ "$TELEGRAM_ONLY" -eq 1 ]]; then
        step_preflight; step_clone_or_update; step_telegram
        return 0
    fi
    if [[ "$OLLAMA_ONLY" -eq 1 ]]; then
        step_preflight; step_clone_or_update; step_ollama_stack
        return 0
    fi

    # Default: full install (preserves prior curl|bash UX)
    step_preflight
    [[ "$CHECK_ONLY" -eq 1 ]] && { ok "check-only mode — exiting"; return 0; }

    step_clone_or_update
    step_cli_install
    step_codesign_and_smoke
    [[ "$NO_ALIASES" -eq 0 ]] && step_aliases
    step_skills_commands
    [[ "$WITH_DOCKER"    -eq 1 ]] && step_ollama_stack
    [[ "$WITH_LAUNCHD"   -eq 1 ]] && step_launchd
    [[ "$WITH_MACOS_APP" -eq 1 ]] && step_macos_app
    step_doctor

    echo
    do_verify || warn "post-install verify reported issues (rc=$?)"

    ok "bootstrap complete"
    log "next: open a new shell so the aliases take effect, then 'cd <your-repo> && crabcc index'"
}

# Sourceable: tests use `BOOTSTRAP_LIB_ONLY=1 source bootstrap.sh` to load
# functions only. Executed: just runs main "$@".
if [[ "${BOOTSTRAP_LIB_ONLY:-0}" != "1" ]]; then
    main "$@"
fi
