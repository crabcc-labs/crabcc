#!/usr/bin/env bash
# tools/crabcc-cron/lib/picker.sh
#
# Picks one eligible issue across the upstream working set.
# Strategy: across all upstreams, pick the lowest issue number on the
# upstream with the highest star count. Ties broken alphabetically.
#
# Depends on: lib/upstream.sh (in caller scope), lib/eligibility.sh.
# Honors $OSS_FIX_STATE_DIR for per-issue state files
# (key: <owner>--<repo>--<issue>.attempted).

# Print JSON object {repo:"owner/name", issue:{...}, stars: N} on stdout,
# or empty string if no eligible issue exists.
pick_issue() {
  local r issues issue n stars best_stars="" best_payload=""
  while IFS= read -r r; do
    [[ -z "$r" ]] && continue
    issues="$(gh issue list \
      --repo "$r" \
      --label "good first issue,help wanted,E-easy,D-easy" \
      --state open \
      --json number,title,labels,assignees,linkedBranches,createdAt,updatedAt,comments 2>/dev/null \
      || echo '[]')"
    stars="$(gh repo view "$r" --json stargazerCount 2>/dev/null | jq -r '.stargazerCount // 0')"

    # Filter to eligible issues, sort ascending by number, take first.
    while IFS= read -r issue; do
      [[ -z "$issue" ]] && continue
      n="$(jq -r '.number' <<<"$issue")"
      [[ -f "${OSS_FIX_STATE_DIR}/${r//\//--}--${n}.attempted" ]] && continue
      if issue_is_eligible "$issue"; then
        if [[ -z "$best_stars" || "$stars" -gt "$best_stars" ]]; then
          best_stars="$stars"
          best_payload="$(jq -nc --arg repo "$r" --argjson stars "$stars" --argjson issue "$issue" \
            '{repo:$repo, stars:$stars, issue:$issue}')"
          # Only need the lowest-numbered eligible issue per repo, hence break.
        fi
        break
      fi
    done < <(jq -c 'sort_by(.number) | .[]' <<<"$issues")
  done < <(upstream_working_set)

  [[ -n "$best_payload" ]] && printf '%s' "$best_payload"
  return 0
}
