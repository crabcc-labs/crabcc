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
  local r owner repo issues issue n stars best_stars="" best_payload="" gql_result
  while IFS= read -r r; do
    [[ -z "$r" ]] && continue
    owner="${r%%/*}"
    repo="${r#*/}"
    # Single GraphQL query replaces two REST calls per repo:
    # `gh issue list` + `gh repo view --json stargazerCount` → one round-trip.
    # Issues are filtered by label (OR) and sorted by number ascending server-side.
    gql_result="$(gh api graphql \
      -f query='query($owner:String!,$repo:String!){
        repository(owner:$owner,name:$repo){
          stargazerCount
          issues(states:[OPEN],
                 labels:["good first issue","help wanted","E-easy","D-easy"],
                 first:30,orderBy:{field:NUMBER,direction:ASC}){
            nodes{
              number title createdAt updatedAt
              labels{nodes{name}}
              assignees{nodes{login}}
              comments{totalCount}
              linkedBranches{nodes{ref{name}}}
            }
          }
        }
      }' \
      -f owner="$owner" -f repo="$repo" 2>/dev/null || echo '{}')"
    stars="$(echo "$gql_result" | jq -r '.data.repository.stargazerCount // 0')"
    # Reshape GraphQL nodes to match the JSON shape expected by eligibility.sh.
    issues="$(echo "$gql_result" | jq -c '
      [.data.repository.issues.nodes // [] | .[] | {
        number, title, createdAt, updatedAt,
        labels:        [.labels.nodes[]       | {name}],
        assignees:     [.assignees.nodes[]    | {login}],
        comments:      {totalCount: .comments.totalCount},
        linkedBranches:[.linkedBranches.nodes[] | {ref:{name:.ref.name}}]
      }]')"

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
