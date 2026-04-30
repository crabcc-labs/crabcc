#!/usr/bin/env bash
# bootstrap.sh — one-shot crabcc setup for a fresh macOS / Linux machine.
#
# Designed to be `curl | bash`-able. Idempotent: same script for fresh
# install, update, and upgrade. Stages the repo under ~/workspace/bin/crabcc
# (configurable via $CRABCC_HOME).
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
#   7. Optional Docker / Ollama stack — only if --with-docker is passed.
#   8. Optional macOS LaunchAgent — only if --with-launchd is passed (macOS).
#   9. Run `scripts/doctor.sh --quiet` and print summary.
#
# Flags:
#   --with-docker     install Docker Desktop (brew cask) + bring up
#                     install/ollama-stack/docker-compose.yml
#   --with-launchd    register the crabcc-agentd LaunchAgent (macOS)
#   --with-macos-app  build + open the .dmg (macOS only)
#   --branch <name>   clone a non-main branch (useful for testing)
#   --check-only      preflight only — no writes
#   --no-aliases      skip step 5
#   --help, -h        this header
#
# Exit codes:
#   0  success
#   1  preflight failed
#   2  bad invocation

set -uo pipefail

# --- defaults -------------------------------------------------------------

CRABCC_HOME="${CRABCC_HOME:-$HOME/workspace/bin/crabcc}"
REPO_URL="${CRABCC_REPO_URL:-https://github.com/peterlodri-sec/crabcc.git}"
BRANCH="main"

WITH_DOCKER=0
WITH_LAUNCHD=0
WITH_MACOS_APP=0
NO_ALIASES=0
CHECK_ONLY=0

OS="$(uname -s)"
IS_MAC=0; [[ "$OS" == "Darwin" ]] && IS_MAC=1

# --- arg parsing ----------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case "$1" in
        --with-docker)    WITH_DOCKER=1 ;;
        --with-launchd)   WITH_LAUNCHD=1 ;;
        --with-macos-app) WITH_MACOS_APP=1 ;;
        --branch)         BRANCH="$2"; shift ;;
        --check-only)     CHECK_ONLY=1 ;;
        --no-aliases)     NO_ALIASES=1 ;;
        --help|-h)        sed -n '1,40p' "$0"; exit 0 ;;
        *)                printf 'unknown flag: %s\n' "$1" >&2; exit 2 ;;
    esac
    shift
done

# --- helpers --------------------------------------------------------------

log()  { printf '\033[1;34m▶\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m✓\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

# --- 1. preflight ---------------------------------------------------------

log "preflight: checking required tools"

have git  || die "git is required (xcode-select --install on macOS, apt install git on Debian)"
have curl || die "curl is required"

# rustup / cargo
if ! have cargo; then
    if [[ $CHECK_ONLY -eq 1 ]]; then
        warn "cargo missing (would install rustup)"
    else
        log "installing rustup (cargo not found)"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
        # shellcheck disable=SC1091
        . "$HOME/.cargo/env"
    fi
fi

# Add ~/.cargo/bin to PATH for the rest of this script.
export PATH="$HOME/.cargo/bin:$PATH"

if [[ $IS_MAC -eq 1 ]]; then
    have codesign || warn "codesign missing — install Xcode Command Line Tools"
fi

ok "preflight passed"
[[ $CHECK_ONLY -eq 1 ]] && { ok "check-only mode — exiting"; exit 0; }

# --- 2. clone or update ---------------------------------------------------

mkdir -p "$(dirname "$CRABCC_HOME")"
if [[ -d "$CRABCC_HOME/.git" ]]; then
    log "updating $CRABCC_HOME (branch: $BRANCH)"
    git -C "$CRABCC_HOME" fetch --tags origin
    git -C "$CRABCC_HOME" checkout "$BRANCH"
    # Pull only when the working tree is clean — otherwise warn + skip.
    if [[ -z "$(git -C "$CRABCC_HOME" status --porcelain)" ]]; then
        git -C "$CRABCC_HOME" pull --rebase --autostash origin "$BRANCH"
        ok "updated to $(git -C "$CRABCC_HOME" rev-parse --short HEAD)"
    else
        warn "$CRABCC_HOME has uncommitted changes — skipping pull"
    fi
else
    log "cloning $REPO_URL (branch: $BRANCH) into $CRABCC_HOME"
    git clone --branch "$BRANCH" --depth 50 "$REPO_URL" "$CRABCC_HOME"
    ok "cloned"
fi

cd "$CRABCC_HOME"

# --- 3. cargo install -----------------------------------------------------

log "cargo install --path crates/crabcc-cli (this may take ~30-60s on first run)"
cargo install --path crates/crabcc-cli --quiet
ok "installed crabcc + ccc to $HOME/.cargo/bin"

# --- 4. macOS ad-hoc codesign --------------------------------------------

if [[ $IS_MAC -eq 1 ]] && have codesign; then
    for b in crabcc ccc; do
        p="$HOME/.cargo/bin/$b"
        [[ -x "$p" ]] || continue
        /usr/bin/codesign --force --sign - "$p" 2>/dev/null \
            && log "codesigned $p (ad-hoc)" \
            || warn "codesign failed for $p"
    done
fi

"$HOME/.cargo/bin/crabcc" --version >/dev/null \
    && ok "crabcc runs ($(crabcc --version))" \
    || die "crabcc failed to run after install"

# --- 5. shell aliases -----------------------------------------------------

if [[ $NO_ALIASES -eq 0 ]]; then
    log "installing shell aliases"
    bash "$CRABCC_HOME/scripts/install-aliases.sh" --all-shells \
        && ok "aliases installed (zsh + bash)" \
        || warn "alias install returned non-zero"
fi

# --- 6. skills + slash commands -------------------------------------------

CL_SKILLS="$HOME/.claude/skills"
CL_CMDS="$HOME/.claude/commands"
mkdir -p "$CL_SKILLS" "$CL_CMDS"

for s in "$CRABCC_HOME/skill"/*/; do
    [[ -d "$s" ]] || continue
    name="$(basename "$s")"
    mkdir -p "$CL_SKILLS/$name"
    for f in "$s"*.md; do
        [[ -e "$f" ]] || continue
        ln -sfn "$f" "$CL_SKILLS/$name/$(basename "$f")"
    done
done
ok "skills linked into $CL_SKILLS"

for c in "$CRABCC_HOME/commands"/*; do
    [[ -e "$c" ]] || continue
    if [[ -d "$c" ]]; then
        name="$(basename "$c")"
        mkdir -p "$CL_CMDS/$name"
        for f in "$c"/*.md; do
            [[ -e "$f" ]] || continue
            ln -sfn "$f" "$CL_CMDS/$name/$(basename "$f")"
        done
    else
        ln -sfn "$c" "$CL_CMDS/$(basename "$c")"
    fi
done
ok "slash commands linked into $CL_CMDS"

# --- 7. Docker / Ollama stack (opt-in) -----------------------------------

if [[ $WITH_DOCKER -eq 1 ]]; then
    log "Docker setup requested"
    if ! have docker; then
        if [[ $IS_MAC -eq 1 ]]; then
            if have brew; then
                brew install --cask docker
                ok "Docker Desktop installed via Homebrew"
                warn "you may need to launch Docker.app once to grant privacy permissions"
            else
                warn "Homebrew not found — install from https://docker.com manually"
            fi
        else
            warn "Linux: install Docker via your package manager (apt/dnf/pacman)"
        fi
    else
        ok "docker already on PATH"
    fi
    compose_file="$CRABCC_HOME/install/ollama-stack/docker-compose.yml"
    if [[ -f "$compose_file" ]] && have docker; then
        log "starting Ollama stack (docker compose up -d)"
        (cd "$(dirname "$compose_file")" && docker compose up -d --wait) \
            && ok "Ollama stack running" \
            || warn "docker compose failed"
    else
        warn "no docker-compose.yml at $compose_file — skipping stack startup"
    fi
fi

# --- 8. macOS LaunchAgent (opt-in) ---------------------------------------

if [[ $WITH_LAUNCHD -eq 1 ]] && [[ $IS_MAC -eq 1 ]]; then
    log "registering crabcc-agentd LaunchAgent"
    bash "$CRABCC_HOME/scripts/install-macos-helpers.sh" \
        && ok "agentd registered" \
        || warn "agentd registration failed"
fi

# --- 9. macOS DMG / .app (opt-in) ----------------------------------------

if [[ $WITH_MACOS_APP -eq 1 ]] && [[ $IS_MAC -eq 1 ]]; then
    log "building Crabcc.dmg (this rebuilds release binaries)"
    bash "$CRABCC_HOME/scripts/build-dmg.sh" \
        && open "$CRABCC_HOME/dist/" \
        || warn "DMG build failed"
fi

# --- 10. doctor summary --------------------------------------------------

log "running doctor"
bash "$CRABCC_HOME/scripts/doctor.sh" --quiet || warn "doctor reported issues — see output above"

ok "bootstrap complete"
log "next: open a new shell so the aliases take effect, then 'cd <your-repo> && crabcc index'"
