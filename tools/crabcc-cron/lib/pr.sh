#!/usr/bin/env bash
# tools/crabcc-cron/lib/pr.sh
#
# Rate-limit gates and PR opening.

: "${OSS_FIX_STATE_DIR:=/opt/crabcc-cron-state/oss-fix}"
: "${OSS_FIX_GLOBAL_CAP:=3}"
: "${OSS_FIX_UPSTREAM_COOLDOWN_DAYS:=7}"

# Returns 0 iff the number of open agent-drafted PRs is >= cap.
global_cap_reached() {
  local count
  count=$(gh pr list \
    --author "@me" \
    --state open \
    --search 'in:body "automated agent"' \
    --json number 2>/dev/null | jq 'length')
  (( count >= OSS_FIX_GLOBAL_CAP ))
}

# Returns 0 iff the upstream has had a PR opened within
# OSS_FIX_UPSTREAM_COOLDOWN_DAYS.
upstream_cap_reached() {
  local repo="$1"
  local f="${OSS_FIX_STATE_DIR}/${repo//\//--}.last_pr"
  [[ -f "$f" ]] || return 1
  local mtime now age
  if [[ "$(uname)" == "Darwin" ]]; then
    mtime=$(stat -f %m "$f")
  else
    mtime=$(stat -c %Y "$f")
  fi
  now=$(date +%s)
  age=$(( (now - mtime) / 86400 ))
  (( age < OSS_FIX_UPSTREAM_COOLDOWN_DAYS ))
}

# Args: sandbox_dir, repo, issue_json
# Pushes the branch, opens a draft PR, returns PR URL on stdout, or empty on failure.
open_draft_pr() {
  local dir="$1" repo="$2" issue="$3"
  local n branch title log_tail body
  n="$(jq -r '.number' <<<"$issue")"
  branch="claude-cron/fix-${n}"
  title="$(jq -r '.title' <<<"$issue")"
  log_tail="$(tail -50 "$dir/opencode.log")"
  # shellcheck disable=SC2016  # %s placeholders are filled by printf args, not shell expansion
  body="$(printf 'Closes #%s\n\n---\nThis PR was drafted by an automated agent (opencode + %s) running on cron. I will review and finalize before requesting merge.\n\n<details><summary>opencode.log (last 50 lines)</summary>\n\n```\n%s\n```\n\n</details>\n' \
    "$n" "${OPENCODE_MODEL:-deepseek-v4-pro}" "$log_tail")"

  (cd "$dir/clone" && git push origin "$branch")
  gh pr create \
    --repo "$repo" \
    --base main \
    --head "$branch" \
    --draft \
    --title "[draft] fix: $title" \
    --body "$body" 2>/dev/null
}

# Args: repo
# Touches the per-upstream cooldown file.
mark_upstream_pr() {
  local repo="$1"
  mkdir -p "$OSS_FIX_STATE_DIR"
  touch "${OSS_FIX_STATE_DIR}/${repo//\//--}.last_pr"
}
