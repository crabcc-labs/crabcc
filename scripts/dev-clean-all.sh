#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/dev-clean-all.sh
#
# Comprehensive workspace cleanup + sync + dependency bootstrap.
# Called by `task dev:clean:all`.
#
# Stages (in order):
#   1. stash         git stash any dirty working tree
#   2. prune         remove .worktrees/ entries older than AGE_DAYS (default 7)
#                    and delete the corresponding remote branches
#   3. sync          git fetch --all --prune && git pull --ff-only
#   4. hooks         scripts/install-hooks.sh --force
#   5. deps          check + install: gh jq rg bat mise task act cargo docker opencode
#   6. summary       structured one-liner + system fingerprint
#
# Usage:
#   scripts/dev-clean-all.sh              # default: prune worktrees > 7 days
#   scripts/dev-clean-all.sh --age 14     # keep worktrees up to 14 days
#   scripts/dev-clean-all.sh --dry-run    # print what would happen, touch nothing
#   scripts/dev-clean-all.sh --no-install # skip dep installation (check only)
#   scripts/dev-clean-all.sh --no-pull    # skip fetch+pull
#   scripts/dev-clean-all.sh -h           # this help
#
# Bypass hooks on git ops: set CRABCC_SKIP_HOOKS=1 in your env.
# ---------------------------------------------------------------------------

set -uo pipefail

# ── args ─────────────────────────────────────────────────────────────────────
AGE_DAYS=7
DRY_RUN=0
NO_INSTALL=0
NO_PULL=0

while [ $# -gt 0 ]; do
    case "$1" in
        --age)       shift; AGE_DAYS="${1:-7}" ;;
        --dry-run)   DRY_RUN=1 ;;
        --no-install) NO_INSTALL=1 ;;
        --no-pull)   NO_PULL=1 ;;
        -h|--help)
            sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "dev-clean-all: unknown arg '$1'" >&2; exit 2 ;;
    esac
    shift
done

# ── env ──────────────────────────────────────────────────────────────────────
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "dev-clean-all: not in a git repo" >&2; exit 1
}
cd "$REPO_ROOT"

OUT_DIR=".summary"
mkdir -p "$OUT_DIR"
LOG="$OUT_DIR/dev-clean-all.log"
: >"$LOG"

TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# ── terminal styling ──────────────────────────────────────────────────────────
if [ -t 1 ] && command -v tput >/dev/null 2>&1; then
    BOLD="$(tput bold 2>/dev/null || true)"
    GREEN="$(tput setaf 2 2>/dev/null || true)"
    YELLOW="$(tput setaf 3 2>/dev/null || true)"
    DIM="$(tput dim 2>/dev/null || true)"
    CYAN="$(tput setaf 6 2>/dev/null || true)"
    RESET="$(tput sgr0 2>/dev/null || true)"
else
    BOLD="" GREEN="" YELLOW="" DIM="" CYAN="" RESET=""
fi

BAR="${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
say()  { printf "%s\n" "$*" | tee -a "$LOG"; }
note() { printf " ${DIM}·${RESET} %s\n" "$*" | tee -a "$LOG"; }
ok()   { printf " ${GREEN}✓${RESET} %s\n" "$*" | tee -a "$LOG"; }
warn() { printf " ${YELLOW}⚠${RESET} %s\n" "$*" | tee -a "$LOG"; }
dry()  { printf " ${CYAN}[dry-run]${RESET} %s\n" "$*" | tee -a "$LOG"; }
run()  {
    if [ "$DRY_RUN" = "1" ]; then dry "$*"; return 0; fi
    printf "  + %s\n" "$*" >> "$LOG"
    "$@" >> "$LOG" 2>&1
}

# ── OS detection ─────────────────────────────────────────────────────────────
detect_os() {
    case "$(uname -s)" in
        Darwin) echo "macos" ;;
        Linux)
            if   [ -f /etc/debian_version ];  then echo "debian"
            elif [ -f /etc/fedora-release ];   then echo "fedora"
            elif [ -f /etc/arch-release ];     then echo "arch"
            elif [ -f /etc/alpine-release ];   then echo "alpine"
            else echo "linux"
            fi ;;
        *) echo "unknown" ;;
    esac
}
OS_ID="$(detect_os)"

# ── install-hint table ────────────────────────────────────────────────────────
# Returns a one-liner that installs TOOL on OS_ID, or empty string if unknown.
install_hint() {
    local tool="$1"
    case "${OS_ID}::${tool}" in
        # gh ──────────────────────────────────────────────────────────────────
        macos::gh)        echo "brew install gh" ;;
        debian::gh)       echo "type -p curl >/dev/null || sudo apt install curl -y; curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | sudo dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg && chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg && echo 'deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main' | sudo tee /etc/apt/sources.list.d/github-cli.list > /dev/null && sudo apt update && sudo apt install gh -y" ;;
        fedora::gh)       echo "sudo dnf install -y gh" ;;
        arch::gh)         echo "sudo pacman -S --needed github-cli" ;;
        # jq ──────────────────────────────────────────────────────────────────
        macos::jq)        echo "brew install jq" ;;
        debian::jq)       echo "sudo apt-get install -y jq" ;;
        fedora::jq)       echo "sudo dnf install -y jq" ;;
        arch::jq)         echo "sudo pacman -S --needed jq" ;;
        alpine::jq)       echo "apk add --no-cache jq" ;;
        # rg (ripgrep) ────────────────────────────────────────────────────────
        macos::rg)        echo "brew install ripgrep" ;;
        debian::rg)       echo "sudo apt-get install -y ripgrep" ;;
        fedora::rg)       echo "sudo dnf install -y ripgrep" ;;
        arch::rg)         echo "sudo pacman -S --needed ripgrep" ;;
        alpine::rg)       echo "apk add --no-cache ripgrep" ;;
        # bat ─────────────────────────────────────────────────────────────────
        macos::bat)       echo "brew install bat" ;;
        debian::bat)      echo "sudo apt-get install -y bat && (command -v batcat && sudo ln -sf \$(which batcat) /usr/local/bin/bat || true)" ;;
        fedora::bat)      echo "sudo dnf install -y bat" ;;
        arch::bat)        echo "sudo pacman -S --needed bat" ;;
        alpine::bat)      echo "apk add --no-cache bat" ;;
        # mise (rtk — formerly rtx) ───────────────────────────────────────────
        macos::mise)      echo "brew install mise" ;;
        *::mise)          echo "curl https://mise.run | sh && echo 'eval \"\$(~/.local/bin/mise activate bash)\"' >> ~/.bashrc" ;;
        # task (go-task) ──────────────────────────────────────────────────────
        macos::task)      echo "brew install go-task" ;;
        *::task)          echo "sh -c \"\$(curl --location https://taskfile.dev/install.sh)\" -- -d -b ~/.local/bin" ;;
        # act (akt — nektos/act) ───────────────────────────────────────────────
        macos::act)       echo "brew install act" ;;
        debian::act)      echo "curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/nektos/act/master/install.sh | sudo bash" ;;
        *::act)           echo "curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/nektos/act/master/install.sh | sudo bash" ;;
        # cargo / rust ────────────────────────────────────────────────────────
        *::cargo)         echo "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path && source \"\$HOME/.cargo/env\"" ;;
        # docker ──────────────────────────────────────────────────────────────
        macos::docker)    echo "brew install --cask docker" ;;
        debian::docker)   echo "curl -fsSL https://get.docker.com | sudo sh && sudo usermod -aG docker \$USER" ;;
        fedora::docker)   echo "sudo dnf install -y docker-ce docker-ce-cli containerd.io && sudo systemctl enable --now docker" ;;
        *::docker)        echo "curl -fsSL https://get.docker.com | sudo sh" ;;
        # opencode ────────────────────────────────────────────────────────────
        macos::opencode)  echo "brew install opencode" ;;
        *::opencode)      echo "npm install -g @opencode-ai/opencode 2>/dev/null || curl -fsSL https://opencode.ai/install | sh" ;;
        *) echo "" ;;
    esac
}

# ── dep check + install ───────────────────────────────────────────────────────
# Returns 0 if tool is on PATH, 1 if not.
dep_present() {
    local tool="$1"
    case "$tool" in
        # Some tools have alternate binary names.
        bat)   command -v bat    >/dev/null 2>&1 || command -v batcat >/dev/null 2>&1 ;;
        mise)  command -v mise   >/dev/null 2>&1 || command -v rtx    >/dev/null 2>&1 ;;
        act)   command -v act    >/dev/null 2>&1 ;;
        cargo) command -v cargo  >/dev/null 2>&1 ;;
        *)     command -v "$tool" >/dev/null 2>&1 ;;
    esac
}

# Maps user-facing "rtk"→"mise", "akt"→"act" short names to canonical ones.
canonical_dep() {
    case "$1" in rtk) echo "mise" ;; akt) echo "act" ;; *) echo "$1" ;; esac
}

# ─────────────────────────────────────────────────────────────────────────────
# STAGE 1: stash
# ─────────────────────────────────────────────────────────────────────────────
say ""
say "${BAR}"
say " ${BOLD}dev:clean:all${RESET}  ${DIM}$TS${RESET}"
say "${BAR}"
say ""
say "${BOLD}[1/5] stash${RESET}"

STASH_REF=""
if git diff --quiet 2>/dev/null && git diff --cached --quiet 2>/dev/null; then
    note "working tree clean — nothing to stash"
else
    STASH_MSG="dev:clean:all  $TS"
    if [ "$DRY_RUN" = "1" ]; then
        dry "git stash push -m \"$STASH_MSG\""
    else
        if git stash push -m "$STASH_MSG" >> "$LOG" 2>&1; then
            STASH_REF="$(git stash list --max-count=1 | cut -d: -f1)"
            ok "stashed as $STASH_REF  (restore: git stash pop)"
        else
            warn "git stash failed — continuing without stash"
        fi
    fi
fi

# ─────────────────────────────────────────────────────────────────────────────
# STAGE 2: prune old worktrees
# ─────────────────────────────────────────────────────────────────────────────
say ""
say "${BOLD}[2/5] prune worktrees  (older than ${AGE_DAYS}d)${RESET}"

DELETED_WORKTREES=()
DELETED_BRANCHES=()
FREED_BYTES=0

if [ -d ".worktrees" ]; then
    # Collect candidate directories older than AGE_DAYS.
    mapfile -t CANDIDATES < <(
        find .worktrees -maxdepth 1 -mindepth 1 -type d -mtime "+${AGE_DAYS}" 2>/dev/null || true
    )

    if [ "${#CANDIDATES[@]}" -eq 0 ]; then
        note "no worktrees older than ${AGE_DAYS}d"
    fi

    for wt_path in "${CANDIDATES[@]}"; do
        [ -d "$wt_path" ] || continue
        SLUG="$(basename "$wt_path")"

        # Resolve the branch name from the worktree's HEAD.
        BRANCH="$(git -C "$wt_path" symbolic-ref --short HEAD 2>/dev/null \
                  || git -C "$wt_path" rev-parse --abbrev-ref HEAD 2>/dev/null \
                  || echo "")"

        # Measure size before deletion.
        WT_BYTES="$(du -sb "$wt_path" 2>/dev/null | cut -f1 || echo 0)"
        FREED_BYTES=$(( FREED_BYTES + WT_BYTES ))

        if [ "$DRY_RUN" = "1" ]; then
            dry "git worktree remove --force $wt_path"
            [ -n "$BRANCH" ] && dry "git push origin --delete $BRANCH"
            DELETED_WORKTREES+=("$SLUG")
            [ -n "$BRANCH" ] && DELETED_BRANCHES+=("$BRANCH")
            continue
        fi

        # Remove the worktree.
        if git worktree remove --force "$wt_path" >> "$LOG" 2>&1 \
                || { rm -rf "$wt_path" && git worktree prune >> "$LOG" 2>&1; }; then
            DELETED_WORKTREES+=("$SLUG")
            ok "removed worktree  $SLUG"
        else
            warn "could not remove worktree $wt_path — skipping"
            continue
        fi

        # Delete the remote branch if it exists.
        if [ -n "$BRANCH" ]; then
            if git ls-remote --exit-code origin "refs/heads/$BRANCH" >/dev/null 2>&1; then
                if CRABCC_SKIP_HOOKS=1 git push origin --delete "$BRANCH" >> "$LOG" 2>&1; then
                    DELETED_BRANCHES+=("$BRANCH")
                    note "deleted remote  origin/$BRANCH"
                else
                    warn "could not delete remote branch origin/$BRANCH"
                fi
            fi
        fi
    done
else
    note ".worktrees/ does not exist — nothing to prune"
fi

FREED_MB=$(( FREED_BYTES / 1048576 ))

# ─────────────────────────────────────────────────────────────────────────────
# STAGE 3: fetch + pull
# ─────────────────────────────────────────────────────────────────────────────
say ""
say "${BOLD}[3/5] fetch + pull${RESET}"

PULLED_COUNT=0

if [ "$NO_PULL" = "1" ]; then
    note "skipped (--no-pull)"
else
    BEFORE_SHA="$(git rev-parse HEAD 2>/dev/null || echo "")"

    if [ "$DRY_RUN" = "1" ]; then
        dry "git fetch --all --prune"
        dry "git pull --ff-only"
    else
        if git fetch --all --prune >> "$LOG" 2>&1; then
            ok "git fetch --all --prune"
        else
            warn "fetch failed — check network / remote access"
        fi

        CURR_BRANCH="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")"
        if git pull --ff-only >> "$LOG" 2>&1; then
            AFTER_SHA="$(git rev-parse HEAD 2>/dev/null || echo "")"
            if [ -n "$BEFORE_SHA" ] && [ "$BEFORE_SHA" != "$AFTER_SHA" ]; then
                PULLED_COUNT="$(git rev-list --count "${BEFORE_SHA}..${AFTER_SHA}" 2>/dev/null || echo 0)"
            fi
            ok "git pull --ff-only  (+${PULLED_COUNT} commits on $CURR_BRANCH)"
        else
            note "pull --ff-only skipped (non-fast-forward or already up to date)"
        fi
    fi
fi

# ─────────────────────────────────────────────────────────────────────────────
# STAGE 4: install hooks
# ─────────────────────────────────────────────────────────────────────────────
say ""
say "${BOLD}[4/5] install hooks${RESET}"

if [ "$DRY_RUN" = "1" ]; then
    dry "scripts/install-hooks.sh --force"
elif bash scripts/install-hooks.sh --force >> "$LOG" 2>&1; then
    HOOK_LIST="$(ls scripts/git-hooks/ | grep -v '\.' | tr '\n' ' ' | sed 's/ $//')"
    ok "hooks installed  [$HOOK_LIST]"
else
    warn "install-hooks.sh failed — check $LOG"
fi

# ─────────────────────────────────────────────────────────────────────────────
# STAGE 5: deps
# ─────────────────────────────────────────────────────────────────────────────
say ""
say "${BOLD}[5/5] deps  (OS: $OS_ID)${RESET}"

# Canonical dep list. Short names the user typed → canonical tool name.
# Order: fastest-to-check first.
USER_DEPS=(gh jq rg bat rtk task akt cargo docker opencode)
DEPS_PRESENT=()
DEPS_INSTALLED=()
DEPS_FAILED=()
DEPS_SKIPPED=()

for user_name in "${USER_DEPS[@]}"; do
    tool="$(canonical_dep "$user_name")"
    label="$user_name"
    [ "$user_name" != "$tool" ] && label="$user_name($tool)"

    if dep_present "$tool"; then
        VER=""
        case "$tool" in
            gh)       VER="$(gh --version 2>/dev/null | head -1 | awk '{print $3}')" ;;
            jq)       VER="$(jq --version 2>/dev/null)" ;;
            rg)       VER="$(rg --version 2>/dev/null | head -1 | awk '{print $2}')" ;;
            bat)      VER="$(bat --version 2>/dev/null | awk '{print $2}' || batcat --version 2>/dev/null | awk '{print $2}')" ;;
            mise)     VER="$(mise --version 2>/dev/null | head -1 | awk '{print $1}' || rtx --version 2>/dev/null | head -1)" ;;
            task)     VER="$(task --version 2>/dev/null | awk '{print $NF}')" ;;
            act)      VER="$(act --version 2>/dev/null | awk '{print $3}')" ;;
            cargo)    VER="$(cargo --version 2>/dev/null | awk '{print $2}')" ;;
            docker)   VER="$(docker --version 2>/dev/null | awk '{print $3}' | tr -d ',')" ;;
            opencode) VER="$(opencode --version 2>/dev/null | head -1 | awk '{print $NF}')" ;;
        esac
        note "$label  ${DIM}${VER:-present}${RESET}"
        DEPS_PRESENT+=("$tool")
        continue
    fi

    # Not present — try to install unless --no-install.
    if [ "$NO_INSTALL" = "1" ]; then
        HINT="$(install_hint "$tool")"
        warn "$label  not found  ${DIM}install: ${HINT:-unknown}${RESET}"
        DEPS_SKIPPED+=("$tool")
        continue
    fi

    HINT="$(install_hint "$tool")"
    if [ -z "$HINT" ]; then
        warn "$label  not found  (no install hint for OS '$OS_ID')"
        DEPS_FAILED+=("$tool")
        continue
    fi

    printf " ${YELLOW}↓${RESET} installing %s  %s\n" "$label" "${DIM}${HINT}${RESET}"

    if [ "$DRY_RUN" = "1" ]; then
        dry "eval: $HINT"
        DEPS_INSTALLED+=("$tool")
        continue
    fi

    if eval "$HINT" >> "$LOG" 2>&1; then
        # Source cargo env in case we just installed rust.
        [ "$tool" = "cargo" ] && { source "$HOME/.cargo/env" 2>/dev/null || true; }
        if dep_present "$tool"; then
            ok "$label  installed"
            DEPS_INSTALLED+=("$tool")
        else
            warn "$label  install ran but binary still not on PATH"
            DEPS_FAILED+=("$tool")
        fi
    else
        warn "$label  install failed — see $LOG"
        DEPS_FAILED+=("$tool")
    fi
done

# ─────────────────────────────────────────────────────────────────────────────
# SYSTEM FINGERPRINT
# ─────────────────────────────────────────────────────────────────────────────
SYS_ID="$(id -un 2>/dev/null || echo "${USER:-?}")"
SYS_CWD="$REPO_ROOT"
SYS_TIME="$(date +"%l:%M%p" | tr -d ' ')"  # e.g. 2:34PM

# OS name
if command -v sw_vers >/dev/null 2>&1; then
    SYS_OS="macOS $(sw_vers -productVersion 2>/dev/null)"
elif [ -f /etc/os-release ]; then
    SYS_OS="$(. /etc/os-release && echo "${PRETTY_NAME:-${NAME:-Linux}}")"
else
    SYS_OS="$(uname -s) $(uname -r)"
fi

SYS_KERNEL="$(uname -r)"

# Uptime
if [ -f /proc/uptime ]; then
    SYS_UPTIME="$(awk '{s=$1; d=int(s/86400); h=int((s%86400)/3600); m=int((s%3600)/60);
        if(d>0) printf "%dd %dh %dm",d,h,m;
        else if(h>0) printf "%dh %dm",h,m;
        else printf "%dm",m}' /proc/uptime)"
elif command -v uptime >/dev/null 2>&1; then
    SYS_UPTIME="$(uptime -p 2>/dev/null | sed 's/^up //' || uptime | sed 's/.*up //' | sed 's/,.*//')"
else
    SYS_UPTIME="?"
fi

# Listening ports (numeric, deduped, top 15)
if command -v ss >/dev/null 2>&1; then
    SYS_PORTS="$(ss -tlnH 2>/dev/null \
        | awk '{split($4,a,":"); p=a[length(a)]; if(p+0>0) print p}' \
        | sort -nu | head -15 | awk '{printf ":%s ",$1}' | sed 's/ $//')"
elif command -v netstat >/dev/null 2>&1; then
    SYS_PORTS="$(netstat -tlnp 2>/dev/null \
        | awk '/LISTEN/ {split($4,a,":"); p=a[length(a)]; if(p+0>0) print p}' \
        | sort -nu | head -15 | awk '{printf ":%s ",$1}' | sed 's/ $//')"
elif command -v lsof >/dev/null 2>&1; then
    SYS_PORTS="$(lsof -i TCP -P -n 2>/dev/null \
        | awk '/LISTEN/ {split($9,a,":"); p=a[length(a)]; if(p+0>0) print p}' \
        | sort -nu | head -15 | awk '{printf ":%s ",$1}' | sed 's/ $//')"
else
    SYS_PORTS="?"
fi

# ─────────────────────────────────────────────────────────────────────────────
# SUMMARY
# ─────────────────────────────────────────────────────────────────────────────
N_DELETED="${#DELETED_WORKTREES[@]}"
N_INSTALLED="${#DEPS_INSTALLED[@]}"

# Build comma-separated lists.
join_arr() { local IFS=", "; echo "$*"; }
WT_LIST=""; [ "$N_DELETED" -gt 0 ] && WT_LIST="  [$(join_arr "${DELETED_WORKTREES[@]}")]"
INST_LIST="[]"; [ "$N_INSTALLED" -gt 0 ] && INST_LIST="[$(join_arr "${DEPS_INSTALLED[@]}")]"
PORT_LIST="[${SYS_PORTS:-none}]"

DRY_TAG=""; [ "$DRY_RUN" = "1" ] && DRY_TAG="  ${YELLOW}(dry-run — no changes made)${RESET}"

say ""
say "${BAR}"
say " ${BOLD}dev:clean:all  summary${RESET}${DRY_TAG}"
say "${BAR}"
printf " %-16s %s\n"  "deleted"      "${N_DELETED} worktrees${WT_LIST}" | tee -a "$LOG"
printf " %-16s %s\n"  "pulled"       "${PULLED_COUNT} commits" | tee -a "$LOG"
printf " %-16s %s\n"  "installed"    "${INST_LIST}" | tee -a "$LOG"
printf " %-16s %s\n"  "space freed"  "${FREED_MB} MB" | tee -a "$LOG"
say ""
printf " %-16s %s\n"  "id"           "${SYS_ID}" | tee -a "$LOG"
printf " %-16s %s\n"  "cwd"          "${SYS_CWD}" | tee -a "$LOG"
printf " %-16s %s\n"  "system_time"  "${SYS_TIME}" | tee -a "$LOG"
printf " %-16s %s\n"  "os"           "${SYS_OS}" | tee -a "$LOG"
printf " %-16s %s\n"  "kernel_ver"   "${SYS_KERNEL}" | tee -a "$LOG"
printf " %-16s %s\n"  "uptime"       "${SYS_UPTIME}" | tee -a "$LOG"
printf " %-16s %s\n"  "open_ports"   "${PORT_LIST}" | tee -a "$LOG"
say "${BAR}"

# Write a compact machine-readable line for tooling / CI consumers.
printf "deleted=%d worktrees,pulled=%d commits,installed=%s,space_freed=%d MB,[id=%s,cwd=%s,system_time=%s,os=%s,kernel_ver=%s,uptime=%s,open_ports=%s]\n" \
    "$N_DELETED" "$PULLED_COUNT" "$INST_LIST" "$FREED_MB" \
    "$SYS_ID" "$SYS_CWD" "$SYS_TIME" \
    "$SYS_OS" "$SYS_KERNEL" "$SYS_UPTIME" "$PORT_LIST" \
    > "$OUT_DIR/dev-clean-all.summary"

say ""
say "  log:     $LOG"
say "  summary: $OUT_DIR/dev-clean-all.summary"

if [ "${#DEPS_FAILED[@]}" -gt 0 ]; then
    say ""
    warn "install failed for: $(join_arr "${DEPS_FAILED[@]}")"
    warn "see $LOG for details"
    say ""
    exit 1
fi

exit 0
