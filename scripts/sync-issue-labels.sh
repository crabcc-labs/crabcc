#!/usr/bin/env bash
# sync-issue-labels.sh — backfill labels on existing issues to match the taxonomy
# defined in .github/LABELS.md.
#
# Idempotent: only adds labels (gh issue edit --add-label). Never removes anything.
# Re-runs are safe.
#
# Usage:
#   scripts/sync-issue-labels.sh           # apply
#   scripts/sync-issue-labels.sh --dry-run # show planned edits
#
# Source of truth for the rules: issues #236–#242 (see LABELS.md).

set -euo pipefail

REPO="${REPO:-peterlodri-sec/crabcc}"
DRY="${1:-}"

run() {
  if [[ "$DRY" == "--dry-run" ]]; then
    echo "DRY: $*"
  else
    eval "$@"
  fi
}

# ---- 1. Ensure the label vocabulary exists -----------------------------------

ensure_label() {
  local name="$1" color="$2" desc="$3"
  if gh label list --repo "$REPO" --limit 200 --json name --jq '.[].name' | grep -Fxq "$name"; then
    return 0
  fi
  run gh label create "'$name'" --color "'$color'" --description "'$desc'" --repo "'$REPO'"
}

ensure_label "rfc"      "8b00ff" "Design proposal needing discussion before code"
ensure_label "refactor" "c5def5" "Code shape change with no behavior change"
ensure_label "test"     "0e8a16" "Test infra or new tests"
ensure_label "chore"    "ededed" "Maintenance work that doesn't fit other categories"

# ---- 2. Backfill labels on under-labeled open issues -------------------------

# Format: ISSUE_NUMBER:LABEL,LABEL,LABEL
# Derived from open-issues triage on $(date -u +%Y-%m-%d) against #236–#242 pattern.
PLAN=(
  "213:enhancement,feature,epic"
  "210:enhancement,feature"
  "204:enhancement,feature"
  "192:enhancement,rfc"
  "189:enhancement,feature"
  "186:feature"
  "184:feature"
  "175:ci,test"
  "173:feature"
  "172:feature,docs"
  "165:feature"
  "164:feature"
  "163:security"
  "160:feature,security"
  "159:feature"
  "157:feature"
  "153:feature"
  "146:feature,docs"
)

for entry in "${PLAN[@]}"; do
  num="${entry%%:*}"
  labels="${entry#*:}"
  echo "→ #$num  +[$labels]"
  run gh issue edit "$num" --repo "$REPO" --add-label "'$labels'"
done

# ---- 3. Re-print the under-labeled list for visual diff ----------------------

echo
echo "Post-sync state:"
gh issue list --repo "$REPO" --state open --limit 200 \
  --json number,title,labels \
  --jq '.[] | "#\(.number) [\(.labels | map(.name) | join(","))] \(.title)"' \
  | sort -t'#' -k2 -n -r
