#!/usr/bin/env bash
# tools/crabcc-cron/lib/agent.sh
#
# Renders the prompt, invokes opencode, parses the outcome.

: "${OPENCODE_MODEL:=deepseek-v4-pro}"
: "${OSS_FIX_MAX_TOKENS:=200000}"
: "${OSS_FIX_TIMEOUT:=30m}"

# Args: template_path, repo, issue_json (whole jq record), test_cmd, out_prompt_path
agent_render_prompt() {
  local tpl="$1" repo="$2" issue="$3" test_cmd="$4" out="$5"
  local n title body
  n="$(jq -r '.number' <<<"$issue")"
  title="$(jq -r '.title' <<<"$issue")"
  body="$(jq -r '.body // ""' <<<"$issue")"
  sed -e "s|{N}|$n|g" \
      -e "s|{repo}|$repo|g" \
      -e "s|{title}|${title//|/\\|}|g" \
      -e "s|{test_cmd}|${test_cmd//|/\\|}|g" \
      "$tpl" \
    | awk -v body="$body" '/^Body:$/ {print; print body; next} {print}' \
    > "$out"
}

# Args: sandbox_dir, prompt_path
# Returns: exit code of opencode, captures stdout to sandbox/opencode.log
agent_run() {
  local dir="$1" prompt="$2"
  timeout "$OSS_FIX_TIMEOUT" opencode run \
    --model "$OPENCODE_MODEL" \
    --cwd "$dir/clone" \
    --prompt-file "$prompt" \
    --max-tokens "$OSS_FIX_MAX_TOKENS" \
    > "$dir/opencode.log" 2>&1
  return $?
}

# Args: log_path, exit_code
# Echoes one of: fixed | tests-failed | no-fix | timeout | error
parse_outcome() {
  local log="$1" exit_code="$2"
  local status_line
  status_line="$(grep -E '^STATUS=' "$log" 2>/dev/null | tail -1 || true)"
  if [[ -n "$status_line" ]]; then
    case "$status_line" in
      STATUS=fixed*)        echo "fixed" ;;
      STATUS=tests-failed*) echo "tests-failed" ;;
      STATUS=no-fix*)       echo "no-fix" ;;
      *)                    echo "error" ;;
    esac
    return 0
  fi
  # No STATUS line.
  if [[ "$exit_code" -eq 124 ]]; then
    echo "timeout"
  else
    echo "error"
  fi
  return 0
}
