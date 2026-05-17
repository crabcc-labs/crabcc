#!/usr/bin/env bash
# tools/crabcc-cron/lib/log.sh
#
# JSONL log + finding emitters. Workloads use these instead of bare `echo`.

log_line() {
  local level="$1"; shift
  jq -nc --arg level "$level" --arg msg "$*" '{kind:"log", level:$level, msg:$msg}'
}
log_info()  { log_line info  "$@"; }
log_warn()  { log_line warn  "$@"; }
log_error() { log_line error "$@"; }

# Args: severity, workload, repo, title, body, metadata_json
emit_finding() {
  local severity="$1" workload="$2" repo="$3" title="$4" body="$5"
  # ${6:-{}} would be mis-parsed by bash (closing brace ambiguity), so split.
  local meta="${6:-}"
  [[ -z "$meta" ]] && meta='{}'
  jq -nc \
    --arg sev "$severity" \
    --arg wl  "$workload" \
    --arg repo "$repo" \
    --arg title "$title" \
    --arg body "$body" \
    --argjson meta "$meta" \
    '{kind:"finding", workload:$wl, repo:$repo, severity:$sev, title:$title, body:$body, metadata:$meta}'
}
