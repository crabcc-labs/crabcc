#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/doctor.sh
#
# Diagnose + repair the crabcc developer-side install:
#
#   - `crabcc` CLI binary on PATH (version probe)
#   - latest published release (compares via `gh`)
#   - MCP server entry in ~/.claude.json
#   - slash-command symlinks under ~/.claude/commands/
#   - skill symlink under ~/.claude/skills/crabcc/
#   - Claude Code hooks under ~/.claude/settings.json
#   - smoke-test: `crabcc index` against a tempdir
#
# Modes:
#   --check      diagnose only (default; no writes)
#   --upgrade    runs `crabcc upgrade --apply` if newer release exists
#   --install    creates / repairs MCP + commands + skill + hooks
#                 entries (idempotent; safe to re-run)
#   --json       machine-readable status
#   --quiet      no banner, single-line statuses, never prompt
#   --help, -h   this header
#
# All runs append to .summary/doctor-YYYYMMDDHHMMSS.log so you can paste
# the trace into a bug report.
#
# Exit codes:
#   0   green: every check passed
#   1   warning: at least one check failed and was not auto-repaired
#   2   bad invocation (unknown flag, etc.)
#
# ---------------------------------------------------------------------------
# CHANGELOG
#   v1.0.0 (2026-04-30) — initial cut. Covers CLI / MCP / commands / skill /
#                          hooks / smoke; --upgrade calls crabcc upgrade;
#                          --install recreates symlinks + MCP entry.
# ---------------------------------------------------------------------------

set -uo pipefail
# NB: no `set -e` here — doctor must keep running past failed checks so the
# user sees the full picture in one run.

# --- terminal styling ------------------------------------------------------
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

# --- arg parsing -----------------------------------------------------------
MODE="check"
JSON=0
QUIET=0
for arg in "$@"; do
    case "$arg" in
        --check)   MODE="check" ;;
        --upgrade) MODE="upgrade" ;;
        --install) MODE="install" ;;
        --json)    JSON=1 ;;
        --quiet)   QUIET=1 ;;
        --help|-h)
            sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown arg: $arg (try --help)" >&2
            exit 2
            ;;
    esac
done

# --- repo root + log file -------------------------------------------------
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOG_DIR="$REPO_ROOT/.summary"
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/doctor-$(date -u +%Y%m%d%H%M%SZ).log"

# Tee everything (incl. command output) to the log file. We intentionally
# don't `exec >| tee` so JSON mode can stay clean stdout.
log() {
    printf "%s\n" "$*" >>"$LOG"
}

# Print + log.  CHK <indent> <name> <status> <detail...>
chk() {
    local sym="$1" color="$2" name="$3" detail="$4"
    log "[$(date -u +%H:%M:%SZ)] $sym $name :: $detail"
    if [ "$JSON" = "0" ] && [ "$QUIET" = "0" ]; then
        printf "  ${color}%s${RESET} %-22s ${DIM}%s${RESET}\n" "$sym" "$name" "$detail"
    elif [ "$QUIET" = "1" ]; then
        printf "  %s %-22s %s\n" "$sym" "$name" "$detail"
    fi
}

# --- pre-flight banner -----------------------------------------------------
if [ "$JSON" = "0" ]; then
    if [ "$QUIET" = "0" ]; then
        printf "${BOLD}crabcc doctor${RESET}  ${DIM}(mode: %s, log: %s)${RESET}\n\n" \
            "$MODE" "$LOG"
    fi
fi
log "=== crabcc doctor — mode=$MODE host=$(uname -srm) ==="

# --- helpers --------------------------------------------------------------
declare -a JSON_ENTRIES=()
fail_count=0

record_json() {
    local name="$1" status="$2" detail="$3"
    # Crude JSON escaping — replace " and \ in detail.
    local escaped="${detail//\\/\\\\}"
    escaped="${escaped//\"/\\\"}"
    JSON_ENTRIES+=("{\"name\":\"$name\",\"status\":\"$status\",\"detail\":\"$escaped\"}")
}

# --- check: crabcc CLI on PATH --------------------------------------------
if command -v crabcc >/dev/null 2>&1; then
    CRABCC_PATH="$(command -v crabcc)"
    CRABCC_VER="$(crabcc --version 2>/dev/null | head -1)"
    chk "✓" "$GREEN" "crabcc-cli" "$CRABCC_VER ($CRABCC_PATH)"
    record_json crabcc-cli ok "$CRABCC_VER"
else
    CRABCC_PATH=""
    CRABCC_VER=""
    chk "✗" "$RED" "crabcc-cli" "not on PATH — install via \`task install\` or cargo install --path crates/crabcc-cli"
    record_json crabcc-cli missing "not on PATH"
    fail_count=$((fail_count + 1))
fi

# --- check: latest GitHub release ---------------------------------------
if command -v gh >/dev/null 2>&1 && [ -n "$CRABCC_VER" ]; then
    LATEST="$(gh release list --repo peterlodri-sec/crabcc --limit 1 2>/dev/null | awk 'NR==1{print $1}')"
    if [ -n "$LATEST" ]; then
        # Strip leading "v" from both for comparison.
        LATEST_NUM="${LATEST#v}"
        CUR_NUM="$(echo "$CRABCC_VER" | awk '{print $2}')"
        if [ -z "$CUR_NUM" ]; then CUR_NUM="(unknown)"; fi
        if [ "$LATEST_NUM" = "$CUR_NUM" ]; then
            chk "✓" "$GREEN" "release-version" "up to date ($LATEST)"
            record_json release-version ok "$LATEST"
        else
            chk "!" "$YELLOW" "release-version" "current $CUR_NUM, latest $LATEST"
            record_json release-version stale "current=$CUR_NUM latest=$LATEST"
        fi
    else
        chk "?" "$DIM" "release-version" "gh release list returned nothing (private repo? auth?)"
        record_json release-version unknown "gh release list empty"
    fi
else
    chk "-" "$DIM" "release-version" "skipped (gh missing or crabcc not installed)"
    record_json release-version skipped "gh missing or crabcc not installed"
fi

# --- check: ~/.claude.json MCP entry --------------------------------------
CLAUDE_JSON="$HOME/.claude.json"
if [ -f "$CLAUDE_JSON" ]; then
    if command -v jq >/dev/null 2>&1; then
        # Search for any object with "command" containing "crabcc-mcp" (CLI
        # subcommand: `crabcc mcp`) or "name" == "crabcc".
        if jq -e '
            ..
            | (objects | select(.command? // "" | test("crabcc")))
            // (objects | select(.name? // "" | test("^crabcc$")))
            ' "$CLAUDE_JSON" >/dev/null 2>&1; then
            chk "✓" "$GREEN" "mcp-registered" "found crabcc entry in ~/.claude.json"
            record_json mcp-registered ok "found crabcc entry"
        else
            chk "✗" "$YELLOW" "mcp-registered" "no crabcc MCP entry in ~/.claude.json"
            record_json mcp-registered missing "no crabcc entry"
            fail_count=$((fail_count + 1))
        fi
    else
        chk "?" "$DIM" "mcp-registered" "skipped (jq missing — install for thorough check)"
        record_json mcp-registered skipped "jq missing"
    fi
else
    chk "✗" "$YELLOW" "mcp-registered" "~/.claude.json not present (Claude Code never run?)"
    record_json mcp-registered missing "~/.claude.json absent"
    fail_count=$((fail_count + 1))
fi

# --- check: slash-command symlinks ----------------------------------------
COMMAND_DIR="$HOME/.claude/commands"
expected_commands=( crabcc-init.md )
[ -d "$REPO_ROOT/commands" ] && {
    # Pick up everything in commands/ as expected, not just the seed name.
    expected_commands=()
    while IFS= read -r f; do
        expected_commands+=("$(basename "$f")")
    done < <(find "$REPO_ROOT/commands" -maxdepth 1 -name '*.md' -type f 2>/dev/null)
}
missing_cmds=()
for cmd in "${expected_commands[@]}"; do
    if [ ! -e "$COMMAND_DIR/$cmd" ]; then
        missing_cmds+=("$cmd")
    fi
done
if [ ${#missing_cmds[@]} -eq 0 ]; then
    chk "✓" "$GREEN" "slash-commands" "all commands symlinked under ~/.claude/commands/"
    record_json slash-commands ok "all commands present"
else
    chk "✗" "$YELLOW" "slash-commands" "missing: ${missing_cmds[*]}"
    record_json slash-commands missing "${missing_cmds[*]}"
    fail_count=$((fail_count + 1))
fi

# --- check: skill symlink -------------------------------------------------
SKILL_LINK="$HOME/.claude/skills/crabcc/SKILL.md"
if [ -e "$SKILL_LINK" ]; then
    chk "✓" "$GREEN" "skill" "$SKILL_LINK present"
    record_json skill ok "linked"
else
    chk "✗" "$YELLOW" "skill" "skill not linked at $SKILL_LINK"
    record_json skill missing "skill not linked"
    fail_count=$((fail_count + 1))
fi

# --- check: hooks (Claude Code settings) ---------------------------------
SETTINGS_JSON="$HOME/.claude/settings.json"
if [ -f "$SETTINGS_JSON" ]; then
    if command -v jq >/dev/null 2>&1; then
        n="$(jq '[.hooks // {} | to_entries[] | .value | length] | add // 0' "$SETTINGS_JSON" 2>/dev/null || echo 0)"
        chk "✓" "$GREEN" "hooks" "$n hook(s) configured in ~/.claude/settings.json"
        record_json hooks ok "$n hooks"
    else
        chk "?" "$DIM" "hooks" "skipped (jq missing)"
        record_json hooks skipped "jq missing"
    fi
else
    chk "-" "$DIM" "hooks" "no ~/.claude/settings.json (no hooks configured)"
    record_json hooks empty "no settings.json"
fi

# --- smoke: index ---------------------------------------------------------
if [ -n "$CRABCC_PATH" ]; then
    SMOKE_DIR="$(mktemp -d -t crabcc-doctor.XXXXXX)"
    cat >"$SMOKE_DIR/a.ts" <<'EOF'
export function hello(name: string) { return name; }
hello("doctor");
EOF
    if (cd "$SMOKE_DIR" && "$CRABCC_PATH" index >/dev/null 2>>"$LOG"); then
        if "$CRABCC_PATH" --root "$SMOKE_DIR" sym hello 2>>"$LOG" | grep -q '"hello"'; then
            chk "✓" "$GREEN" "smoke-index" "indexed + sym lookup OK"
            record_json smoke-index ok "indexed + sym lookup OK"
        else
            chk "✗" "$RED" "smoke-index" "sym lookup did not return 'hello'"
            record_json smoke-index failed "sym lookup empty"
            fail_count=$((fail_count + 1))
        fi
    else
        chk "✗" "$RED" "smoke-index" "crabcc index failed (see $LOG)"
        record_json smoke-index failed "crabcc index error"
        fail_count=$((fail_count + 1))
    fi
    rm -rf "$SMOKE_DIR"
else
    chk "-" "$DIM" "smoke-index" "skipped (no crabcc on PATH)"
    record_json smoke-index skipped "no crabcc"
fi

# --- repair actions (only run for --install / --upgrade) ------------------
do_install() {
    [ "$QUIET" = "0" ] && printf "\n${BOLD}repair: --install${RESET}\n"

    mkdir -p "$HOME/.claude/skills/crabcc" "$HOME/.claude/commands"

    # Slash commands.
    if [ -d "$REPO_ROOT/commands" ]; then
        for f in "$REPO_ROOT/commands"/*.md; do
            [ -e "$f" ] || continue
            ln -sf "$f" "$HOME/.claude/commands/$(basename "$f")"
            log "linked $(basename "$f")"
        done
        chk "✓" "$GREEN" "linked-commands" "$(ls "$HOME/.claude/commands" | wc -l | tr -d ' ') file(s)"
    fi

    # Skill.
    if [ -f "$REPO_ROOT/skill/crabcc/SKILL.md" ]; then
        ln -sf "$REPO_ROOT/skill/crabcc/SKILL.md" "$HOME/.claude/skills/crabcc/SKILL.md"
        chk "✓" "$GREEN" "linked-skill" "$HOME/.claude/skills/crabcc/SKILL.md"
    else
        chk "!" "$YELLOW" "linked-skill" "no skill file at $REPO_ROOT/skill/crabcc/SKILL.md"
    fi

    # MCP entry. We do NOT mutate ~/.claude.json automatically — touching
    # someone's user config is high-blast-radius. Print the suggested
    # command instead and let the user run it.
    if command -v claude >/dev/null 2>&1; then
        local cmd="claude mcp add crabcc -- crabcc mcp"
        chk "→" "$BLUE" "mcp-suggest" "run: $cmd"
        log "suggested: $cmd"
    else
        chk "!" "$YELLOW" "mcp-suggest" "claude CLI not installed; install Claude Code first"
    fi
}

do_upgrade() {
    [ "$QUIET" = "0" ] && printf "\n${BOLD}repair: --upgrade${RESET}\n"
    if [ -z "$CRABCC_PATH" ]; then
        chk "!" "$YELLOW" "upgrade" "crabcc not installed; nothing to upgrade"
        return
    fi
    log "running: $CRABCC_PATH upgrade --apply"
    if "$CRABCC_PATH" upgrade --apply 2>&1 | tee -a "$LOG"; then
        chk "✓" "$GREEN" "upgrade" "crabcc upgrade --apply ran cleanly"
    else
        chk "✗" "$RED" "upgrade" "crabcc upgrade --apply failed (see $LOG)"
        fail_count=$((fail_count + 1))
    fi
}

case "$MODE" in
    install) do_install ;;
    upgrade) do_upgrade ;;
esac

# --- json mode --------------------------------------------------------------
if [ "$JSON" = "1" ]; then
    printf '{"mode":"%s","log":"%s","tools":[' "$MODE" "$LOG"
    sep=""
    for entry in "${JSON_ENTRIES[@]}"; do
        printf '%s%s' "$sep" "$entry"
        sep=","
    done
    printf '],"failures":%d}\n' "$fail_count"
    exit "$fail_count"
fi

# --- summary ---------------------------------------------------------------
if [ "$QUIET" = "0" ]; then
    if [ "$fail_count" -eq 0 ]; then
        printf "\n${GREEN}all green${RESET} — log: %s\n" "$LOG"
    else
        printf "\n${RED}%d issue(s)${RESET} — log: %s\n" "$fail_count" "$LOG"
        printf "    fix hints: ${DIM}--install${RESET} (re-link commands/skill/MCP) or ${DIM}--upgrade${RESET} (pick up newer release)\n"
    fi
fi

exit "$fail_count"
