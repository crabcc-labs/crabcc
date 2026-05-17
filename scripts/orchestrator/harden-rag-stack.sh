#!/usr/bin/env bash
# Harden peterlodri-sec/rag-stack to match the crabcc baseline:
#   - squash-only merges, PR title/body for squash commit, delete branch on merge, allow update branch
#   - vulnerability alerts enabled
#   - branch ruleset \"protect-main\" on refs/heads/main with deletion + non-fast-forward
#     + required_linear_history + pull_request (1 approval, dismiss stale reviews on push)
#
# Idempotent: PATCH and PUT are safe to re-run. The ruleset POST will return 422 if a
# ruleset with the same name already exists; the script detects that and exits 0.
#
# Requires: gh auth with admin on peterlodri-sec/rag-stack.
set -euo pipefail

REPO=\"peterlodri-sec/rag-stack\"

echo \"==> [1/3] Patching repo merge settings on ${REPO}...\"
gh api \
  --method PATCH \
  -H \"Accept: application/vnd.github+json\" \
  \"repos/${REPO}\" \
  -F allow_squash_merge=true \
  -F allow_merge_commit=false \
  -F allow_rebase_merge=false \
  -f squash_merge_commit_title=PR_TITLE \
  -f squash_merge_commit_message=PR_BODY \
  -F delete_branch_on_merge=true \
  -F allow_update_branch=true \
  >/dev/null

echo \"==> [2/3] Enabling vulnerability alerts on ${REPO}...\"
gh api \
  --method PUT \
  -H \"Accept: application/vnd.github+json\" \
  \"repos/${REPO}/vulnerability-alerts\"

echo \"==> [3/3] Creating branch ruleset 'protect-main' on ${REPO}...\"
RULESET_PAYLOAD=$(cat <<'JSON'
{
  \"name\": \"protect-main\",
  \"target\": \"branch\",
  \"enforcement\": \"active\",
  \"conditions\": {
    \"ref_name\": {
      \"include\": [\"refs/heads/main\"],
      \"exclude\": []
    }
  },
  \"rules\": [
    { \"type\": \"deletion\" },
    { \"type\": \"non_fast_forward\" },
    { \"type\": \"required_linear_history\" },
    {
      \"type\": \"pull_request\",
      \"parameters\": {
        \"required_approving_review_count\": 1,
        \"dismiss_stale_reviews_on_push\": true,
        \"require_code_owner_review\": false,
        \"require_last_push_approval\": false,
        \"required_review_thread_resolution\": false
      }
    }
  ]
}
JSON
)

set +e
RULESET_RESPONSE=$(printf '%s' \"${RULESET_PAYLOAD}\" | gh api \
  --method POST \
  -H \"Accept: application/vnd.github+json\" \
  \"repos/${REPO}/rulesets\" \
  --input - 2>&1)
RULESET_STATUS=$?
set -e

if [ \"${RULESET_STATUS}\" -eq 0 ]; then
  echo \"    ruleset created.\"
elif printf '%s' \"${RULESET_RESPONSE}\" | grep -q \"already exists\"; then
  echo \"    ruleset 'protect-main' already exists; leaving in place.\"
else
  echo \"ERROR: ruleset creation failed:\" >&2
  printf '%s\n' \"${RULESET_RESPONSE}\" >&2
  exit 1
fi

echo
echo \"Done. Hardening applied to ${REPO}.\"
