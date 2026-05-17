#!/usr/bin/env bash
# tools/crabcc-cron/lib/sandbox.sh
#
# Creates and cleans per-attempt sandbox dirs under
# /srv/cron-agents/oss-fix/<owner>--<repo>--<issue>/.

: "${SANDBOX_ROOT:=/srv/cron-agents/oss-fix}"

# Args: repo (owner/name), issue_number
# Echoes the absolute sandbox path; creates clone/ inside.
sandbox_create() {
  local repo="$1" issue="$2"
  local key="${repo//\//--}--${issue}"
  local dir="${SANDBOX_ROOT}/${key}"
  mkdir -p "$dir"
  echo "running" >"$dir/status"
  echo "$dir"
}

# Args: sandbox_dir, repo (owner/name), issue_number
# Clones the upstream into sandbox/clone and creates the working branch.
sandbox_clone() {
  local dir="$1" repo="$2" issue="$3"
  gh repo clone "$repo" "$dir/clone" -- --quiet
  git -C "$dir/clone" checkout -b "claude-cron/fix-${issue}"
}

# Args: sandbox_dir, final_status
sandbox_finalize() {
  local dir="$1" status="$2"
  echo "$status" >"$dir/status"
}
