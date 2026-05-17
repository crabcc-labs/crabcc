#!/usr/bin/env bash
# tools/crabcc-cron/lib/eligibility.sh
#
# Pure-function predicate over an issue JSON record.
# Returns 0 iff the issue is eligible per spec §4.3.
#
# Honors $NOW (ISO 8601 string) for deterministic testing; defaults to
# current time.

# Convert ISO 8601 string to unix seconds.
_iso_to_epoch() {
  if [[ "$(uname)" == "Darwin" ]]; then
    date -j -f "%Y-%m-%dT%H:%M:%SZ" "$1" +%s 2>/dev/null
  else
    date -d "$1" +%s 2>/dev/null
  fi
}

_now_epoch() {
  if [[ -n "${NOW:-}" ]]; then
    _iso_to_epoch "$NOW"
  else
    date +%s
  fi
}

issue_is_eligible() {
  local issue="$1"
  local now created updated age_days idle_days assignees linked comments_total

  now="$(_now_epoch)"
  created="$(_iso_to_epoch "$(jq -r '.createdAt' <<<"$issue")")"
  updated="$(_iso_to_epoch "$(jq -r '.updatedAt' <<<"$issue")")"
  age_days=$(( (now - created) / 86400 ))
  idle_days=$(( (now - updated) / 86400 ))
  assignees="$(jq '.assignees | length' <<<"$issue")"
  linked="$(jq '.linkedBranches | length' <<<"$issue")"
  comments_total="$(jq '.comments.totalCount' <<<"$issue")"

  # All gates must pass.
  (( assignees == 0 ))      || return 1
  (( linked == 0 ))         || return 2
  (( age_days >= 7 ))       || return 3
  (( age_days <= 180 ))     || return 4
  (( idle_days >= 30 ))     || return 5
  (( comments_total <= 10 ))|| return 6
  return 0
}
