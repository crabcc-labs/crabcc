#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/dev-local-bump.sh
#
# Semver bump across all versioned files, CHANGELOG, README, then
# triggers a docs-refresh. Called by `task dev:local:bump`.
#
# Usage:
#   scripts/dev-local-bump.sh [patch|minor|major] [options]
#
# Options:
#   patch|minor|major   which component to increment (default: patch)
#   --msg "..."         plain-English description for the CHANGELOG
#                       entry header (e.g. "Redis pub/sub support")
#   --no-commit         update files but don't git commit
#   --no-docs           skip docs-refresh spawn
#   --dry-run           print what would change, touch nothing
#   -h | --help         print this help
#
# Files updated:
#   Cargo.toml                     [workspace.package].version
#   crates/crabcc-viz/Cargo.toml   [package].version (standalone)
#   Cargo.lock                     regenerated via `cargo check`
#   CHANGELOG.md                   version heading inserted after [Unreleased]
#   README.md                      version badge + inline code references
#
# Exit:
#   0   success
#   1   error
#   2   bad usage
# ---------------------------------------------------------------------------

set -uo pipefail

# ── args ─────────────────────────────────────────────────────────────────────
BUMP="patch"
MSG=""
NO_COMMIT=0
NO_DOCS=0
DRY_RUN=0

while [ $# -gt 0 ]; do
    case "$1" in
        patch|minor|major) BUMP="$1" ;;
        --msg)       shift; MSG="${1:-}" ;;
        --no-commit) NO_COMMIT=1 ;;
        --no-docs)   NO_DOCS=1 ;;
        --dry-run)   DRY_RUN=1 ;;
        -h|--help)
            sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "dev-local-bump: unknown arg '$1' (try --help)" >&2; exit 2 ;;
    esac
    shift
done

# Allow Taskfile to pass multi-word strings via env vars.
[ -n "${_BUMP_MSG:-}"       ] && MSG="$_BUMP_MSG"
[ "${_BUMP_NO_COMMIT:-}"  = "1" ] && NO_COMMIT=1
[ "${_BUMP_NO_DOCS:-}"    = "1" ] && NO_DOCS=1
[ "${_BUMP_DRY_RUN:-}"    = "1" ] && DRY_RUN=1

# ── env ──────────────────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

OUT_DIR=".summary"
mkdir -p "$OUT_DIR"

LOG="$OUT_DIR/dev-local-bump.log"
: >"$LOG"

if [ -t 1 ] && command -v tput >/dev/null 2>&1; then
    BOLD="$(tput bold 2>/dev/null || true)"
    GREEN="$(tput setaf 2 2>/dev/null || true)"
    DIM="$(tput dim 2>/dev/null || true)"
    RESET="$(tput sgr0 2>/dev/null || true)"
else
    BOLD="" GREEN="" DIM="" RESET=""
fi

say()  { printf "%s\n" "$*" | tee -a "$LOG"; }
ok()   { printf " ${GREEN}✓${RESET} %s\n" "$*" | tee -a "$LOG"; }
info() { printf " ${DIM}…${RESET} %s\n" "$*" | tee -a "$LOG"; }

# ── read current version ─────────────────────────────────────────────────────
OLD_VER="$(bash scripts/version.sh)" || {
    echo "dev-local-bump: could not read version from Cargo.toml" >&2; exit 1
}

MAJOR="$(echo "$OLD_VER" | cut -d. -f1)"
MINOR="$(echo "$OLD_VER" | cut -d. -f2)"
PATCH="$(echo "$OLD_VER" | cut -d. -f3)"

case "$BUMP" in
    major) NEW_VER="$((MAJOR+1)).0.0" ;;
    minor) NEW_VER="${MAJOR}.$((MINOR+1)).0" ;;
    patch) NEW_VER="${MAJOR}.${MINOR}.$((PATCH+1))" ;;
esac

DATE="$(date +%Y-%m-%d)"

say ""
say "${BOLD}dev:local:bump${RESET}  $OLD_VER → $NEW_VER  ($BUMP)  $DATE"
say ""

if [ "$DRY_RUN" = "1" ]; then
    say "[dry-run] files that would be updated:"
    say "  Cargo.toml                    version = \"$OLD_VER\" → \"$NEW_VER\""
    say "  crates/crabcc-viz/Cargo.toml  version = \"$OLD_VER\" → \"$NEW_VER\""
    say "  Cargo.lock                    (cargo check --workspace)"
    say "  CHANGELOG.md                  insert ## [$NEW_VER] — $DATE"
    say "  README.md                     v$OLD_VER → v$NEW_VER"
    [ "$NO_COMMIT" = "0" ] && say "  git commit  \"chore(release): bump v$OLD_VER → v$NEW_VER\""
    [ "$NO_DOCS"   = "0" ] && say "  docs-refresh (background)"
    exit 0
fi

# ── update Cargo.toml (workspace) ────────────────────────────────────────────
info "Cargo.toml  (workspace.package.version)"
# Only replace the first occurrence that follows [workspace.package]
awk -v old="$OLD_VER" -v new="$NEW_VER" '
    /^\[workspace\.package\]/ { in_section=1 }
    /^\[/ && !/^\[workspace\.package\]/ { in_section=0 }
    in_section && /^version[[:space:]]*=/ && !done {
        gsub("\"" old "\"", "\"" new "\"")
        done=1
    }
    { print }
' Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml
ok "Cargo.toml"

# ── update crabcc-viz/Cargo.toml (standalone package) ────────────────────────
VIZ_TOML="crates/crabcc-viz/Cargo.toml"
if [ -f "$VIZ_TOML" ]; then
    info "$VIZ_TOML  (package.version)"
    awk -v old="$OLD_VER" -v new="$NEW_VER" '
        /^\[package\]/ { in_section=1 }
        /^\[/ && !/^\[package\]/ { in_section=0 }
        in_section && /^version[[:space:]]*=/ && !done {
            gsub("\"" old "\"", "\"" new "\"")
            done=1
        }
        { print }
    ' "$VIZ_TOML" > "$VIZ_TOML.tmp" && mv "$VIZ_TOML.tmp" "$VIZ_TOML"
    ok "$VIZ_TOML"
fi

# ── regenerate Cargo.lock ─────────────────────────────────────────────────────
info "Cargo.lock  (cargo check --workspace)"
if cargo check --workspace --quiet >> "$LOG" 2>&1; then
    ok "Cargo.lock"
else
    say "  warning: cargo check failed — Cargo.lock may be stale (continuing)" | tee -a "$LOG"
fi

# ── update CHANGELOG.md ───────────────────────────────────────────────────────
info "CHANGELOG.md  (insert ## [$NEW_VER] — $DATE)"
HEADING="## [$NEW_VER] — $DATE"
[ -n "$MSG" ] && HEADING="$HEADING  $MSG"

# Collect any content in [Unreleased] (between the heading and the next ## [).
# If empty (just blank lines), the new section gets a placeholder.
UNRELEASED_CONTENT="$(awk '
    /^## \[Unreleased\]/ { found=1; next }
    found && /^## \[/ { exit }
    found { print }
' CHANGELOG.md | sed '/^[[:space:]]*$/d')"

# Rewrite: keep [Unreleased] empty, insert new versioned section below it.
awk -v heading="$HEADING" -v content="$UNRELEASED_CONTENT" '
    /^## \[Unreleased\]/ {
        print
        in_unreleased=1
        next
    }
    in_unreleased && /^## \[/ {
        printf "\n%s\n", heading
        if (content != "") {
            printf "\n%s\n", content
        }
        print ""
        in_unreleased=0
        print
        next
    }
    in_unreleased { next }
    { print }
' CHANGELOG.md > CHANGELOG.md.tmp && mv CHANGELOG.md.tmp CHANGELOG.md
ok "CHANGELOG.md"

# ── update README.md ─────────────────────────────────────────────────────────
if [ -f README.md ]; then
    info "README.md  (v$OLD_VER → v$NEW_VER)"
    # Replace version in shields.io badges, inline code, and version strings.
    sed -i.bak \
        -e "s/v${OLD_VER}/v${NEW_VER}/g" \
        -e "s/version = \"${OLD_VER}\"/version = \"${NEW_VER}\"/g" \
        -e "s/crabcc = \"${OLD_VER}\"/crabcc = \"${NEW_VER}\"/g" \
        README.md
    rm -f README.md.bak
    ok "README.md"
fi

# ── commit ────────────────────────────────────────────────────────────────────
if [ "$NO_COMMIT" = "0" ]; then
    info "git commit"
    FILES_TO_STAGE="Cargo.toml Cargo.lock CHANGELOG.md"
    [ -f "$VIZ_TOML" ] && FILES_TO_STAGE="$FILES_TO_STAGE $VIZ_TOML"
    [ -f "README.md" ] && FILES_TO_STAGE="$FILES_TO_STAGE README.md"
    # shellcheck disable=SC2086
    git add $FILES_TO_STAGE
    COMMIT_MSG="chore(release): bump v$OLD_VER → v$NEW_VER"
    if git commit -m "$COMMIT_MSG" >> "$LOG" 2>&1; then
        SHA="$(git rev-parse --short HEAD)"
        ok "commit $SHA  \"$COMMIT_MSG\""
    else
        say "  error: git commit failed — see $LOG" >&2
        exit 1
    fi
fi

# ── docs-refresh (background) ────────────────────────────────────────────────
if [ "$NO_DOCS" = "0" ]; then
    if command -v claude >/dev/null 2>&1; then
        info "docs-refresh (background — follow: tail -f .summary/docs-refresh.log)"
        bash scripts/docs-refresh.sh 2>/dev/null || \
        nohup claude -p "
You are a docs janitor for crabcc. The version was just bumped from
$OLD_VER to $NEW_VER. Update README.md, AGENTS.md, and CLAUDE.md so
all version references, install instructions, and feature descriptions
match the new version and the CHANGELOG.md [[$NEW_VER]] section.
Preserve existing tone, headings, and ordering. Do NOT touch source files,
schema, or anything under target/.
" --model sonnet >> .summary/docs-refresh.log 2>&1 &
        say "  docs-refresh pid=$!"
    else
        say "  ${DIM}docs-refresh skipped (claude CLI not on PATH)${RESET}"
    fi
fi

say ""
say "${GREEN}${BOLD}✓${RESET} bump complete:  v$OLD_VER → v$NEW_VER"
say "  log: $LOG"
say ""
say "Next steps:"
say "  task dev:local:ship MSG=\"chore(release): cut v$NEW_VER\""
say "  # or: git tag v$NEW_VER && git push origin main --tags"
say ""
