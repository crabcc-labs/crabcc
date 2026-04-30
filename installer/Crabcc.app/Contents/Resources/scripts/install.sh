#!/bin/bash
# install.sh — embedded in Crabcc.app/Contents/Resources.
#
# Steps:
#   1. Copy bundled bin/{crabcc,ccc} to ~/.cargo/bin (or fall back to ~/.local/bin).
#   2. Ad-hoc codesign both — fixes Sequoia provenance-xattr exec denial.
#   3. Symlink skills + commands into ~/.claude/{skills,commands}.
#   4. Run scripts/install-aliases.sh (also bundled) — minimal mode.
#   5. Render LaunchAgent plist with absolute paths and `launchctl bootstrap`.
#   6. Add the Crabcc.app path to the Crabcc agent repos list (so agentd
#      finds at least the parent install repo if launched from there).
#
# Idempotent. Safe to re-run.

set -uo pipefail

RESOURCES="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
APP_DIR="$(cd "$RESOURCES/.." && pwd)"
APP_BUNDLE="$(cd "$APP_DIR/.." && pwd)"
HOME_DIR="${HOME:?HOME unset}"
UID_NUM="$(id -u)"

log()  { printf '[%s] %s\n' "$(date -u '+%H:%M:%SZ')" "$*"; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

# --- 1. binaries ----------------------------------------------------------

BIN_DST="$HOME_DIR/.cargo/bin"
[[ -d "$BIN_DST" ]] || BIN_DST="$HOME_DIR/.local/bin"
mkdir -p "$BIN_DST"

for bin in crabcc ccc; do
    src="$RESOURCES/bin/$bin"
    [[ -f "$src" ]] || die "missing bundled binary: $src"
    install -m 0755 "$src" "$BIN_DST/$bin"
    log "installed $BIN_DST/$bin"
done

# --- 2. ad-hoc codesign (Sequoia provenance fix) --------------------------

for bin in crabcc ccc; do
    /usr/bin/codesign --force --sign - "$BIN_DST/$bin" 2>/dev/null \
        && log "codesigned $bin (ad-hoc)" \
        || log "warn: codesign failed for $bin"
done

# --- 3. skills + slash commands -------------------------------------------

CLAUDE_SKILLS="$HOME_DIR/.claude/skills"
CLAUDE_CMDS="$HOME_DIR/.claude/commands"
mkdir -p "$CLAUDE_SKILLS" "$CLAUDE_CMDS"

if [[ -d "$RESOURCES/skills" ]]; then
    for skill_src in "$RESOURCES/skills"/*; do
        [[ -d "$skill_src" ]] || continue
        name="$(basename "$skill_src")"
        dst="$CLAUDE_SKILLS/$name"
        mkdir -p "$dst"
        for f in "$skill_src"/*; do
            [[ -e "$f" ]] || continue
            ln -sfn "$f" "$dst/$(basename "$f")"
        done
        log "linked skill $name"
    done
fi

if [[ -d "$RESOURCES/commands" ]]; then
    # files at top level
    for f in "$RESOURCES/commands"/*.md; do
        [[ -e "$f" ]] || continue
        ln -sfn "$f" "$CLAUDE_CMDS/$(basename "$f")"
        log "linked command $(basename "$f")"
    done
    # nested (e.g. ccc-init/lazy.md)
    for sub in "$RESOURCES/commands"/*/; do
        [[ -d "$sub" ]] || continue
        name="$(basename "$sub")"
        mkdir -p "$CLAUDE_CMDS/$name"
        for f in "$sub"*.md; do
            [[ -e "$f" ]] || continue
            ln -sfn "$f" "$CLAUDE_CMDS/$name/$(basename "$f")"
        done
        log "linked command group $name"
    done
fi

# --- 4. shell aliases -----------------------------------------------------

if [[ -x "$RESOURCES/install-aliases.sh" ]]; then
    PATH="$BIN_DST:$PATH" bash "$RESOURCES/install-aliases.sh" --all-shells \
        && log "aliases installed (zsh + bash)" \
        || log "warn: alias install returned non-zero"
fi

# --- 5. LaunchAgents (agentd + menubar) -----------------------------------

LA_DIR="$HOME_DIR/Library/LaunchAgents"
mkdir -p "$LA_DIR"

register_launch_agent() {
    local label="$1"; local src="$2"
    local dst="$LA_DIR/$label.plist"
    sed -e "s|__APP_PATH__|$APP_BUNDLE|g" -e "s|__HOME__|$HOME_DIR|g" "$src" > "$dst"
    log "wrote LaunchAgent at $dst"
    if /bin/launchctl print "gui/$UID_NUM/$label" >/dev/null 2>&1; then
        /bin/launchctl bootout "gui/$UID_NUM/$label" 2>/dev/null || true
    fi
    if /bin/launchctl bootstrap "gui/$UID_NUM" "$dst"; then
        /bin/launchctl enable "gui/$UID_NUM/$label" 2>/dev/null || true
        /bin/launchctl kickstart -k "gui/$UID_NUM/$label" 2>/dev/null || true
        log "$label registered with launchd"
    else
        log "warn: bootstrap failed for $label — see Console.app for diagnostics"
    fi
}

register_launch_agent "com.crabcc.manager"     "$RESOURCES/com.crabcc.manager.plist"
register_launch_agent "com.crabcc.agentd"      "$RESOURCES/com.crabcc.agentd.plist"
register_launch_agent "com.crabcc.menubar"     "$RESOURCES/com.crabcc.menubar.plist"
register_launch_agent "com.crabcc.agent-guard" "$RESOURCES/com.crabcc.agent-guard.plist"

# --- 6. seed agent repos.list + agent _internal.db -----------------------

STATE_DIR="$HOME_DIR/.crabcc"
mkdir -p "$STATE_DIR/agent"
touch "$STATE_DIR/agent/repos.list"

# Pre-create the singleton agent-runs DB so the menubar's Status section
# has something to query before any agent has actually run. crabcc itself
# (re)creates / migrates the schema on first write.
if command -v sqlite3 >/dev/null 2>&1 && [[ ! -f "$STATE_DIR/_internal.db" ]]; then
    sqlite3 "$STATE_DIR/_internal.db" <<'SQL' >/dev/null 2>&1 || true
CREATE TABLE IF NOT EXISTS agent_runs (
    id           TEXT PRIMARY KEY,
    started_ts   INTEGER NOT NULL,
    finished_ts  INTEGER,
    pid          INTEGER,
    repo         TEXT NOT NULL,
    runtime      TEXT,
    model        TEXT,
    log_path     TEXT,
    meta_path    TEXT,
    exit_code    INTEGER,
    status       TEXT NOT NULL DEFAULT 'running'
);
CREATE INDEX IF NOT EXISTS idx_agent_runs_started ON agent_runs(started_ts DESC);
CREATE INDEX IF NOT EXISTS idx_agent_runs_status  ON agent_runs(status);
SQL
    log "seeded $STATE_DIR/_internal.db"
fi

log "install complete"
