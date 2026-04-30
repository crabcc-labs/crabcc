#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/check-deps.sh
#
# Portable dev-environment doctor for the crabcc workspace.
#
# Walks a curated list of external tools the build / test / docs / release
# pipeline relies on, prints a one-line status per tool, and (when run
# interactively on a TTY) offers to install anything missing using the
# correct package manager for the host OS.
#
# Buckets:
#   required     — without these, `task default` will not run
#   recommended  — Taskfile.yml smoke / bench / lint targets need these
#   optional     — speed-ups + niceties (rg, fd, yq, claude, repomix, …)
#
# Supported hosts:
#   - macOS (brew)
#   - Linux: Debian/Ubuntu (apt), Fedora/RHEL (dnf), Arch (pacman),
#     Alpine (apk), openSUSE (zypper)
#   - FreeBSD (pkg) — best-effort install hints; status check is
#     guaranteed to work everywhere `command -v` does.
#
# Usage:
#   scripts/check-deps.sh            # interactive: status + install prompts
#   scripts/check-deps.sh --strict   # exit non-zero if anything is missing
#   scripts/check-deps.sh --quiet    # only print status table; never prompt
#   scripts/check-deps.sh --json     # machine-readable status (for CI hooks)
#   scripts/check-deps.sh --help     # this header
#
# Exit codes:
#   0  all required tools present (and optional ones either installed or
#      explicitly skipped); --strict additionally requires every tool.
#   1  required tool missing and user did not install it.
#   2  unsupported host OS (no install hints available).
#
# ---------------------------------------------------------------------------
# CHANGELOG
#   v1.0.0 (2026-04-30) — initial port from the inline check that lived in
#                          install.sh. Adds three buckets, --json output,
#                          per-OS install hint table, version probe.
# ---------------------------------------------------------------------------

set -euo pipefail

# --- workspace version (single source of truth) ----------------------------
__SD="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
# shellcheck disable=SC1091
. "$__SD/version.sh" 2>/dev/null || true
CRABCC_VERSION="${CRABCC_VERSION:-unknown}"

# --- terminal styling -------------------------------------------------------
if [ -t 1 ] && command -v tput >/dev/null 2>&1; then
    BOLD="$(tput bold || true)"
    DIM="$(tput dim || true)"
    RED="$(tput setaf 1 || true)"
    YELLOW="$(tput setaf 3 || true)"
    GREEN="$(tput setaf 2 || true)"
    BLUE="$(tput setaf 4 || true)"
    RESET="$(tput sgr0 || true)"
else
    BOLD=""; DIM=""; RED=""; YELLOW=""; GREEN=""; BLUE=""; RESET=""
fi

# --- arg parsing ------------------------------------------------------------
MODE="interactive"
for arg in "$@"; do
    case "$arg" in
        --strict)  MODE="strict" ;;
        --quiet)   MODE="quiet" ;;
        --json)    MODE="json" ;;
        --help|-h)
            sed -n '2,50p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown arg: $arg (try --help)" >&2
            exit 2
            ;;
    esac
done

# --- OS detection -----------------------------------------------------------
detect_os() {
    case "$(uname -s)" in
        Darwin) echo "macos" ;;
        Linux)
            if [ -f /etc/os-release ]; then
                # shellcheck disable=SC1091
                . /etc/os-release
                case "${ID:-}${ID_LIKE:-}" in
                    *debian*|*ubuntu*) echo "debian" ;;
                    *fedora*|*rhel*|*centos*) echo "fedora" ;;
                    *arch*) echo "arch" ;;
                    *alpine*) echo "alpine" ;;
                    *suse*) echo "suse" ;;
                    *) echo "linux-unknown" ;;
                esac
            else
                echo "linux-unknown"
            fi
            ;;
        FreeBSD) echo "freebsd" ;;
        OpenBSD) echo "openbsd" ;;
        *) echo "unknown" ;;
    esac
}

OS="$(detect_os)"

# --- install-hint table -----------------------------------------------------
# `install_hint TOOL OS` echoes the recommended one-liner. Empty string
# means "no known package; manual install required". The keys are the
# canonical tool names used in the bucket lists below.
install_hint() {
    local tool="$1" os="$2"
    case "$os::$tool" in
        # ----- shells / core -----
        macos::bash)        echo "brew install bash" ;;
        debian::bash)       echo "sudo apt-get install -y bash" ;;
        # ----- build / test -----
        macos::cargo)       echo "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" ;;
        debian::cargo)      echo "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" ;;
        fedora::cargo)      echo "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" ;;
        arch::cargo)        echo "sudo pacman -S --needed rustup && rustup default stable" ;;
        alpine::cargo)      echo "apk add --no-cache cargo" ;;
        # ----- task runner -----
        macos::task)        echo "brew install go-task" ;;
        debian::task)       echo "sh -c \"\$(curl --location https://taskfile.dev/install.sh)\" -- -d -b /usr/local/bin" ;;
        fedora::task)       echo "sh -c \"\$(curl --location https://taskfile.dev/install.sh)\" -- -d -b /usr/local/bin" ;;
        arch::task)         echo "sudo pacman -S --needed go-task" ;;
        # ----- json / yaml / search -----
        macos::jq)          echo "brew install jq" ;;
        debian::jq)         echo "sudo apt-get install -y jq" ;;
        fedora::jq)         echo "sudo dnf install -y jq" ;;
        arch::jq)           echo "sudo pacman -S --needed jq" ;;
        alpine::jq)         echo "apk add --no-cache jq" ;;
        suse::jq)           echo "sudo zypper install -y jq" ;;
        freebsd::jq)        echo "pkg install -y jq" ;;

        macos::yq)          echo "brew install yq" ;;
        debian::yq)         echo "sudo snap install yq || (sudo wget -qO /usr/local/bin/yq https://github.com/mikefarah/yq/releases/latest/download/yq_linux_amd64 && sudo chmod +x /usr/local/bin/yq)" ;;
        fedora::yq)         echo "sudo dnf install -y yq" ;;
        arch::yq)           echo "sudo pacman -S --needed go-yq" ;;
        alpine::yq)         echo "apk add --no-cache yq" ;;

        macos::rg)          echo "brew install ripgrep" ;;
        debian::rg)         echo "sudo apt-get install -y ripgrep" ;;
        fedora::rg)         echo "sudo dnf install -y ripgrep" ;;
        arch::rg)           echo "sudo pacman -S --needed ripgrep" ;;
        alpine::rg)         echo "apk add --no-cache ripgrep" ;;
        suse::rg)           echo "sudo zypper install -y ripgrep" ;;

        macos::fd)          echo "brew install fd" ;;
        debian::fd)         echo "sudo apt-get install -y fd-find && sudo ln -sf $(which fdfind) /usr/local/bin/fd" ;;
        fedora::fd)         echo "sudo dnf install -y fd-find" ;;
        arch::fd)           echo "sudo pacman -S --needed fd" ;;
        alpine::fd)         echo "apk add --no-cache fd" ;;

        # ----- git / GH -----
        macos::git)         echo "brew install git" ;;
        debian::git)        echo "sudo apt-get install -y git" ;;
        fedora::git)        echo "sudo dnf install -y git" ;;
        arch::git)          echo "sudo pacman -S --needed git" ;;
        alpine::git)        echo "apk add --no-cache git" ;;

        macos::gh)          echo "brew install gh" ;;
        debian::gh)         echo "sudo apt-get install -y gh" ;;
        fedora::gh)         echo "sudo dnf install -y gh" ;;
        arch::gh)           echo "sudo pacman -S --needed github-cli" ;;
        alpine::gh)         echo "apk add --no-cache github-cli" ;;

        # ----- python / node -----
        macos::python3)     echo "brew install python@3.12" ;;
        debian::python3)    echo "sudo apt-get install -y python3 python3-pip" ;;
        fedora::python3)    echo "sudo dnf install -y python3 python3-pip" ;;
        arch::python3)      echo "sudo pacman -S --needed python python-pip" ;;

        # ----- claude / repomix (npm-installed) -----
        *::claude)          echo "npm install -g @anthropic-ai/claude-code" ;;
        *::repomix)         echo "npm install -g repomix" ;;

        # ----- fallback -----
        *)                  echo "" ;;
    esac
}

# --- tool buckets -----------------------------------------------------------
# Each line: NAME|BUCKET|DESCRIPTION|VERSION_CMD
# VERSION_CMD is run as `bash -c` and may print to stdout.
tools_table() {
    cat <<'EOF'
bash|required|POSIX-compliant shell (this script needs ≥ 4.0)|bash --version | head -1
git|required|Version control|git --version
cargo|required|Rust toolchain (cargo + rustc)|cargo --version
task|recommended|go-task — runner for Taskfile.yml|task --version
jq|recommended|JSON processor — used by Taskfile smoke targets|jq --version
gh|recommended|GitHub CLI — PR / issue automation|gh --version | head -1
python3|recommended|bench/visualize.py + bench scripts|python3 --version
rg|optional|ripgrep — fast content search|rg --version | head -1
fd|optional|friendlier find — used by ad-hoc dev workflows|fd --version
yq|optional|YAML processor (Taskfile linting)|yq --version
claude|optional|Claude Code CLI — needed for `task docs-refresh`|claude --version
repomix|optional|Codebase packer — needed for `task repomix`|repomix --version
EOF
}

# --- core check loop --------------------------------------------------------
declare -a MISSING_REQ=()
declare -a MISSING_REC=()
declare -a MISSING_OPT=()
declare -a JSON_ENTRIES=()

check_tool() {
    local name="$1" bucket="$2" desc="$3" verscmd="$4"
    local status version path
    if command -v "$name" >/dev/null 2>&1; then
        path="$(command -v "$name")"
        version="$(bash -c "$verscmd" 2>/dev/null | head -1 | tr -d '\n' | sed 's/  */ /g' || true)"
        if [ -z "$version" ]; then version="(present)"; fi
        status="ok"
        if [ "$MODE" != "json" ] && [ "$MODE" != "quiet" ]; then
            printf "  ${GREEN}✓${RESET} %-10s ${DIM}%s${RESET}\n" "$name" "$version"
        elif [ "$MODE" = "quiet" ]; then
            printf "  ok   %-10s %s\n" "$name" "$version"
        fi
    else
        path=""
        version=""
        status="missing"
        case "$bucket" in
            required)    MISSING_REQ+=("$name") ;;
            recommended) MISSING_REC+=("$name") ;;
            optional)    MISSING_OPT+=("$name") ;;
        esac
        local color="$YELLOW"
        [ "$bucket" = "required" ] && color="$RED"
        if [ "$MODE" != "json" ] && [ "$MODE" != "quiet" ]; then
            printf "  ${color}✗${RESET} %-10s ${DIM}%s — %s${RESET}\n" "$name" "$bucket" "$desc"
        elif [ "$MODE" = "quiet" ]; then
            printf "  miss %-10s %s — %s\n" "$name" "$bucket" "$desc"
        fi
    fi
    JSON_ENTRIES+=("{\"name\":\"$name\",\"bucket\":\"$bucket\",\"status\":\"$status\",\"path\":\"$path\",\"version\":\"$version\"}")
}

# --- header / banner --------------------------------------------------------
if [ "$MODE" != "json" ]; then
    printf "${BOLD}crabcc dev-deps check${RESET}  ${DIM}(crabcc v%s, host: %s)${RESET}\n\n" \
        "$CRABCC_VERSION" "$OS"
fi

while IFS='|' read -r name bucket desc verscmd; do
    [ -z "$name" ] && continue
    check_tool "$name" "$bucket" "$desc" "$verscmd"
done < <(tools_table)

# --- json mode --------------------------------------------------------------
if [ "$MODE" = "json" ]; then
    printf '{"os":"%s","tools":[' "$OS"
    sep=""
    for entry in "${JSON_ENTRIES[@]}"; do
        printf '%s%s' "$sep" "$entry"
        sep=","
    done
    printf "]}\n"
    [ ${#MISSING_REQ[@]} -eq 0 ] || exit 1
    exit 0
fi

# --- summary + interactive install -----------------------------------------
all_missing=("${MISSING_REQ[@]}" "${MISSING_REC[@]}" "${MISSING_OPT[@]}")

if [ ${#all_missing[@]} -eq 0 ]; then
    printf "\n${GREEN}all good — every tool is on PATH${RESET}\n"
    exit 0
fi

printf "\n${BOLD}summary:${RESET}  required missing: %d, recommended: %d, optional: %d\n" \
    "${#MISSING_REQ[@]}" "${#MISSING_REC[@]}" "${#MISSING_OPT[@]}"

# In quiet / strict modes we never prompt.
if [ "$MODE" = "quiet" ]; then
    [ ${#MISSING_REQ[@]} -eq 0 ] || exit 1
    exit 0
fi
if [ "$MODE" = "strict" ]; then
    [ ${#all_missing[@]} -eq 0 ] || exit 1
    exit 0
fi

prompt_install_one() {
    local tool="$1" hint
    hint="$(install_hint "$tool" "$OS")"
    if [ -z "$hint" ]; then
        printf "  ${YELLOW}!${RESET} no known install command for %s on %s; please install manually\n" "$tool" "$OS"
        return 1
    fi
    printf "\n  ${BLUE}→${RESET} install %s? ${DIM}[y/N]${RESET} " "$tool"
    printf "    cmd: %s\n  > " "$hint"
    local reply=""
    if [ -t 0 ]; then
        read -r reply
    else
        reply="n"
    fi
    case "$reply" in
        y|Y|yes|YES)
            printf "    running: %s\n" "$hint"
            bash -c "$hint" || {
                printf "  ${RED}install failed${RESET} — see output above\n"
                return 1
            }
            ;;
        *)
            printf "    skipped\n"
            return 1
            ;;
    esac
}

if ! [ -t 0 ]; then
    printf "\n${DIM}(stdin not a tty — skipping interactive install)${RESET}\n"
    [ ${#MISSING_REQ[@]} -eq 0 ] || exit 1
    exit 0
fi

if [ ${#MISSING_REQ[@]} -gt 0 ]; then
    printf "\n${BOLD}required tools missing — these MUST be installed${RESET}\n"
    for t in "${MISSING_REQ[@]}"; do prompt_install_one "$t" || true; done
fi
if [ ${#MISSING_REC[@]} -gt 0 ]; then
    printf "\n${BOLD}recommended tools missing${RESET}\n"
    for t in "${MISSING_REC[@]}"; do prompt_install_one "$t" || true; done
fi
if [ ${#MISSING_OPT[@]} -gt 0 ]; then
    printf "\n${BOLD}optional tools missing${RESET}\n"
    for t in "${MISSING_OPT[@]}"; do prompt_install_one "$t" || true; done
fi

# Re-verify required tools after the install loop. If any are still gone,
# exit non-zero so callers (CI / Taskfile preflight) can react.
still_missing=0
for t in "${MISSING_REQ[@]}"; do
    if ! command -v "$t" >/dev/null 2>&1; then
        printf "  ${RED}✗ %s still missing${RESET}\n" "$t"
        still_missing=1
    fi
done
exit "$still_missing"
