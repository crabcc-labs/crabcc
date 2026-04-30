#!/usr/bin/env bash
# migrate-cli-to-groups.sh
#
# Title: grouping commands into groups · existing groups · cleanup cli · DX
#
# Migrate scripts, dotfiles, and Taskfiles from the 33 top-level crabcc
# commands to the 12-group nested layout introduced in issue #145.
#
# Phases:
#   1. Grouping commands into groups — rewrite `crabcc <old>` → `crabcc <group> <sub>`
#   2. Existing groups — surface what's already nested and untouched
#   3. Cleanup CLI — sweep stale `.bak` files and warn on lingering env vars
#   4. DX — color diffs, dry-run default, --apply only writes with .bak,
#      --cleanup wipes the .baks once you've reviewed them
#
# Usage:
#   ./scripts/migrate-cli-to-groups.sh [--apply] [--quiet] [--cleanup] [PATH...]
#
#   --apply     rewrite files in place (creates <file>.bak alongside each)
#   --quiet     skip per-file diff output, just print the summary
#   --cleanup   delete every *.bak left by previous --apply runs, then exit
#   PATH...     directories or files to scan (default: cwd)
#
# Skips: .git/, target/, node_modules/, .crabcc/, *.bak, binary files.

set -euo pipefail

# ── colors ────────────────────────────────────────────────────────────────
if [[ -t 1 ]] && [[ -z "${NO_COLOR:-}" ]]; then
    BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GRN=$'\033[32m'
    YEL=$'\033[33m'; CYN=$'\033[36m'; RST=$'\033[0m'
else
    BOLD=; DIM=; RED=; GRN=; YEL=; CYN=; RST=
fi

# ── arg parse ─────────────────────────────────────────────────────────────
APPLY=0; QUIET=0; CLEANUP=0; PATHS=()
while (( $# > 0 )); do
    case "$1" in
        --apply)   APPLY=1 ;;
        --quiet)   QUIET=1 ;;
        --cleanup) CLEANUP=1 ;;
        -h|--help)
            awk 'NR == 1 { next } /^[^#]/ { exit } { sub(/^# ?/, ""); print }' "$0"
            exit 0 ;;
        *) PATHS+=("$1") ;;
    esac
    shift
done
[[ ${#PATHS[@]} -eq 0 ]] && PATHS=(.)

# ── phase 3 (run-and-exit) ────────────────────────────────────────────────
if (( CLEANUP == 1 )); then
    n=0
    while IFS= read -r -d '' bak; do
        rm -- "$bak"
        n=$((n + 1))
    done < <(
        for p in "${PATHS[@]}"; do
            find "$p" -type f -name '*.bak' \
                -not -path '*/.git/*' \
                -not -path '*/target/*' \
                -not -path '*/node_modules/*' \
                -print0
        done
    )
    echo "${GRN}✓${RST} cleanup: removed ${n} .bak file(s)"
    exit 0
fi

# ── phase 2 banner: groups already nested ─────────────────────────────────
if (( QUIET == 0 )); then
    cat <<EOF
${BOLD}migrate-cli-to-groups${RST}
  ${DIM}grouping commands into groups · existing groups · cleanup cli · DX${RST}

${DIM}existing nested groups (no migration needed):${RST}
  ${CYN}graph${RST}    build, walk, cycles, orphans
  ${CYN}memory${RST}   remember, search, get, list, …
  ${CYN}doctor${RST}   docker, stack, keys, agent, jobs   ${DIM}(+ new: discovery, all)${RST}
  ${CYN}jobs${RST}     <op>
  ${CYN}backup${RST}   now, ls, restore, prune
  ${CYN}serve${RST}    (singleton)

${DIM}old top-level → new path being rewritten:${RST}
  index → index build               sym → lookup sym
  refresh → index refresh           refs → lookup refs
  fts-rebuild → index fts-rebuild   callers → lookup callers
  watch → index watch               outline → lookup outline
  compress → index compress         files → lookup files
                                    grep → lookup grep
  agent → agent run                 fuzzy → lookup fuzzy
  agent-ls → agent ls               prefix → lookup prefix
  agent-guard → agent guard
  agent-kills → agent kills         ollama-stack → stack
  model-info → agent models         debug-service-discovery → doctor discovery
                                    install-claude → setup install-claude
  upgrade → setup upgrade           track → info track
  completions → setup completions
  openapi → setup openapi
  go → setup go

EOF
fi

# ── phase 1: rewriter mappings ────────────────────────────────────────────
# Hyphenated commands rewrite cleanly with a literal substitution.
HYPHEN_MAP=$(cat <<'EOF'
agent-ls	agent ls
agent-guard	agent guard
agent-kills	agent kills
model-info	agent models
ollama-stack	stack
debug-service-discovery	doctor discovery
install-claude	setup install-claude
fts-rebuild	index fts-rebuild
EOF
)

# Bare-word commands need a negative-lookahead guard so we don't double-rewrite
# already-migrated invocations (`crabcc index build` should stay as-is).
declare -A BARE_GROUP=(
    [index]="index build"
    [refresh]="index refresh"
    [watch]="index watch"
    [compress]="index compress"
    [sym]="lookup sym"
    [refs]="lookup refs"
    [callers]="lookup callers"
    [outline]="lookup outline"
    [files]="lookup files"
    [grep]="lookup grep"
    [fuzzy]="lookup fuzzy"
    [prefix]="lookup prefix"
    [agent]="agent run"
    [upgrade]="setup upgrade"
    [completions]="setup completions"
    [openapi]="setup openapi"
    [go]="setup go"
    [track]="info track"
)

# Negative-lookahead: skip if the next token is already the new subcommand.
declare -A BARE_SKIP=(
    [index]="build|refresh|fts-rebuild|watch|compress"
    [agent]="run|ls|guard|kills|models"
)

# ── phase 1 + 4: per-file scan + diff ─────────────────────────────────────
process_file() {
    local file="$1"
    if file --mime "$file" 2>/dev/null | grep -q 'charset=binary'; then
        return 1
    fi

    local original migrated
    original=$(<"$file")
    migrated=$original

    # Pass A — hyphenated literal rewrites
    while IFS=$'\t' read -r old new; do
        [[ -z "$old" ]] && continue
        migrated=$(printf '%s' "$migrated" | perl -pe "s/\bcrabcc \Q${old}\E\b/crabcc ${new}/g")
    done <<< "$HYPHEN_MAP"

    # Pass B — bare-word rewrites with negative-lookahead guard
    local old new skip
    for old in "${!BARE_GROUP[@]}"; do
        new="${BARE_GROUP[$old]}"
        skip="${BARE_SKIP[$old]:-}"
        if [[ -n "$skip" ]]; then
            migrated=$(printf '%s' "$migrated" \
                | perl -pe "s/\bcrabcc \Q${old}\E(?!\s+(?:${skip})\b)/crabcc ${new}/g")
        else
            migrated=$(printf '%s' "$migrated" \
                | perl -pe "s/\bcrabcc \Q${old}\E\b/crabcc ${new}/g")
        fi
    done

    if [[ "$original" == "$migrated" ]]; then
        return 1
    fi

    if (( QUIET == 0 )); then
        printf '%s%s%s\n' "${BOLD}" "$file" "${RST}"
        diff <(printf '%s' "$original") <(printf '%s' "$migrated") \
            | grep -E '^[<>]' \
            | head -20 \
            | sed -e "s/^</  ${RED}-${RST}/" -e "s/^>/  ${GRN}+${RST}/"
        echo
    fi

    if (( APPLY == 1 )); then
        cp -- "$file" "${file}.bak"
        printf '%s' "$migrated" > "$file"
    fi
    return 0
}

# ── walk + drive ──────────────────────────────────────────────────────────
total=0
changed=0
while IFS= read -r -d '' f; do
    total=$((total + 1))
    if process_file "$f"; then
        changed=$((changed + 1))
    fi
done < <(
    for p in "${PATHS[@]}"; do
        find "$p" -type f \
            -not -path '*/.git/*' \
            -not -path '*/target/*' \
            -not -path '*/node_modules/*' \
            -not -path '*/.crabcc/*' \
            -not -name '*.bak' \
            -not -name '*.pyc' \
            -print0
    done
)

# ── summary ───────────────────────────────────────────────────────────────
echo "${DIM}─────────────────────────────────────${RST}"
if (( APPLY == 1 )); then
    printf '%bapplied%b: %b%d%b file(s) modified, %d scanned\n' \
        "$BOLD" "$RST" "$GRN" "$changed" "$RST" "$total"
    echo "review .bak files, then: ${CYN}$(basename "$0") --cleanup${RST}"
elif (( changed > 0 )); then
    printf '%bdry-run%b: %b%d%b file(s) would change, %d scanned\n' \
        "$BOLD" "$RST" "$YEL" "$changed" "$RST" "$total"
    echo "re-run with ${CYN}--apply${RST} to rewrite (creates .bak per file)"
else
    printf '%b✓%b %d file(s) scanned, no deprecated invocations found\n' \
        "$GRN" "$RST" "$total"
fi
