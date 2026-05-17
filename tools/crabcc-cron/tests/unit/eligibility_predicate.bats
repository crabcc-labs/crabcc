#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  # Source the lib so `issue_is_eligible` is in scope.
  # NOW=ts overrides current-time for deterministic age math.
  export NOW="2026-05-17T00:00:00Z"
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/eligibility.sh"
}
teardown() { teardown_tempdir; }

# helpers: build an issue JSON object with overrides.
mk_issue() {
  jq -nc \
    --arg created "${1:-2026-04-01T00:00:00Z}" \
    --arg updated "${2:-2026-04-01T00:00:00Z}" \
    --argjson assignees "${3:-[]}" \
    --argjson linkedBranches "${4:-[]}" \
    --argjson commentsTotal "${5:-0}" \
    '{createdAt:$created, updatedAt:$updated, assignees:$assignees, linkedBranches:$linkedBranches, comments:{totalCount:$commentsTotal}}'
}

@test "eligibility: passes for fresh, unassigned, untouched, low-comment issue" {
  issue=$(mk_issue "2026-04-01T00:00:00Z" "2026-04-01T00:00:00Z" "[]" "[]" 3)
  run issue_is_eligible "$issue"
  [[ "$status" -eq 0 ]]
}

@test "eligibility: rejects assigned issue" {
  issue=$(mk_issue "2026-04-01T00:00:00Z" "2026-04-01T00:00:00Z" '[{"login":"x"}]' "[]" 0)
  run issue_is_eligible "$issue"
  [[ "$status" -ne 0 ]]
}

@test "eligibility: rejects issue with linked branch (existing PR)" {
  issue=$(mk_issue "2026-04-01T00:00:00Z" "2026-04-01T00:00:00Z" "[]" '[{"name":"fix-x"}]' 0)
  run issue_is_eligible "$issue"
  [[ "$status" -ne 0 ]]
}

@test "eligibility: rejects issue younger than 7d" {
  # 5 days before NOW (2026-05-17)
  issue=$(mk_issue "2026-05-12T00:00:00Z" "2026-05-12T00:00:00Z" "[]" "[]" 0)
  run issue_is_eligible "$issue"
  [[ "$status" -ne 0 ]]
}

@test "eligibility: rejects issue older than 180d" {
  issue=$(mk_issue "2025-11-01T00:00:00Z" "2025-11-01T00:00:00Z" "[]" "[]" 0)
  run issue_is_eligible "$issue"
  [[ "$status" -ne 0 ]]
}

@test "eligibility: rejects issue actively discussed (updated < 30d ago)" {
  # Created 60d ago but updated 15d ago.
  issue=$(mk_issue "2026-03-15T00:00:00Z" "2026-05-02T00:00:00Z" "[]" "[]" 0)
  run issue_is_eligible "$issue"
  [[ "$status" -ne 0 ]]
}

@test "eligibility: rejects issue with > 10 comments" {
  issue=$(mk_issue "2026-04-01T00:00:00Z" "2026-04-01T00:00:00Z" "[]" "[]" 12)
  run issue_is_eligible "$issue"
  [[ "$status" -ne 0 ]]
}
