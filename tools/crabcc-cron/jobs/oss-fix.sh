#!/usr/bin/env bash
# tools/crabcc-cron/jobs/oss-fix.sh - WL-2 dispatcher entrypoint.
#
# One tick = at most one PR attempt. Emits exactly one finding per tick.
#
# Honors:
#   $OSS_FIX_DRY_RUN - if 1, does everything except `git push` and `gh pr create`.

set -uo pipefail

CRON_ROOT="${CRON_ROOT:-/opt/crabcc-cron}"

# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/log.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/eligibility.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/upstream.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/picker.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/sandbox.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/agent.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/pr.sh"

: "${OSS_FIX_CONFIG:=/etc/crabcc-cron/oss-fix.toml}"
: "${OSS_FIX_STATE_DIR:=/opt/crabcc-cron-state/oss-fix}"
mkdir -p "$OSS_FIX_STATE_DIR"

# Load config into shell.
eval "$("${CRON_ROOT}/bin/crabcc-cron-config-shim" "$OSS_FIX_CONFIG")"

t_start=$(date +%s)

log_info "oss-fix tick start"

# Global cap.
if global_cap_reached; then
  log_info "global cap reached, skipping"
  emit_finding info oss-fix "" "at_cap" "Global cap of $OSS_FIX_GLOBAL_CAP open agent-drafted PRs reached." '{}'
  exit 0
fi

# Pick an issue.
pick="$(pick_issue)"
if [[ -z "$pick" ]]; then
  log_info "no eligible issue across upstreams"
  emit_finding info oss-fix "" "no_eligible_issue" "No upstream had an eligible issue this tick." '{}'
  exit 0
fi

repo="$(jq -r '.repo' <<<"$pick")"
issue="$(jq -c '.issue' <<<"$pick")"
n="$(jq -r '.number' <<<"$issue")"

# Per-upstream cap.
if upstream_cap_reached "$repo"; then
  log_info "upstream $repo in cooldown, skipping"
  emit_finding info oss-fix "$repo" "at_cap" "Per-upstream cap reached for $repo." \
    "$(jq -nc --argjson n "$n" '{issue_number:$n}')"
  exit 0
fi

# Sandbox + clone.
dir="$(sandbox_create "$repo" "$n")"
log_info "sandbox: $dir"
if ! sandbox_clone "$dir" "$repo" "$n" 2>>"$dir/opencode.log"; then
  log_error "clone failed"
  sandbox_finalize "$dir" "error"
  touch "${OSS_FIX_STATE_DIR}/${repo//\//--}--${n}.attempted"
  emit_finding error oss-fix "$repo" "clone_failed" \
    "Failed to clone $repo for issue #$n." \
    "$(jq -nc --argjson n "$n" '{issue_number:$n}')"
  exit 0
fi

# Render prompt.
test_cmd="cargo test --workspace"  # MVP: rust-only.
agent_render_prompt \
  "${CRON_ROOT}/templates/oss-fix.md" \
  "$repo" \
  "$issue" \
  "$test_cmd" \
  "$dir/prompt.md"

# Run agent.
log_info "invoking opencode for $repo#$n"
agent_run "$dir" "$dir/prompt.md"
ec=$?

# Parse outcome.
outcome="$(parse_outcome "$dir/opencode.log" "$ec")"
sandbox_finalize "$dir" "$outcome"
touch "${OSS_FIX_STATE_DIR}/${repo//\//--}--${n}.attempted"

duration=$(( $(date +%s) - t_start ))
meta="$(jq -nc --argjson n "$n" --argjson dur "$duration" --arg dir "$dir" --argjson ec "$ec" \
  '{issue_number:$n, attempt_dir:$dir, duration_s:$dur, opencode_exit_code:$ec}')"

case "$outcome" in
  fixed)
    if [[ "${OSS_FIX_DRY_RUN:-0}" -eq 1 ]]; then
      log_info "dry-run: would push branch + open draft PR for $repo#$n"
      emit_finding info oss-fix "$repo" "pr_opened_dryrun" \
        "Dry-run: would open draft PR for $repo#$n." "$meta"
    else
      pr_url="$(open_draft_pr "$dir" "$repo" "$issue" || true)"
      if [[ -n "$pr_url" ]]; then
        mark_upstream_pr "$repo"
        emit_finding info oss-fix "$repo" "pr_opened" \
          "Draft PR opened: $pr_url" \
          "$(jq -nc --argjson n "$n" --argjson dur "$duration" --arg url "$pr_url" \
              '{issue_number:$n, duration_s:$dur, pr_url:$url}')"
      else
        emit_finding error oss-fix "$repo" "pr_open_failed" \
          "Tests passed but PR creation failed." "$meta"
      fi
    fi
    ;;
  tests-failed)
    emit_finding warn oss-fix "$repo" "tests_failed" \
      "Agent attempted fix for $repo#$n; tests did not pass." "$meta"
    ;;
  no-fix)
    emit_finding info oss-fix "$repo" "no_fix" \
      "Agent declined to fix $repo#$n (unclear / out of scope)." "$meta"
    ;;
  timeout)
    emit_finding warn oss-fix "$repo" "timeout" \
      "Agent timed out on $repo#$n." "$meta"
    ;;
  *)
    emit_finding error oss-fix "$repo" "error" \
      "Agent crashed or exited without STATUS on $repo#$n." "$meta"
    ;;
esac

log_info "oss-fix tick done in ${duration}s (outcome=$outcome)"
exit 0
