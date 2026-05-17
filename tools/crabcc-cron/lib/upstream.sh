#!/usr/bin/env bash
# tools/crabcc-cron/lib/upstream.sh
#
# Computes the working set of upstreams for this tick.
# Reads TIER2_INCLUDE[] and TIER3_EXCLUDE[] from the environment
# (set by `eval "$(crabcc-cron-config-shim ...)"`).
#
# Tier1 auto-discovery via gh repo list is deferred to a follow-up plan.

upstream_working_set() {
  local r excluded
  for r in "${TIER2_INCLUDE[@]:-}"; do
    [[ -z "$r" ]] && continue
    excluded=0
    for ex in "${TIER3_EXCLUDE[@]:-}"; do
      [[ "$ex" == "$r" ]] && { excluded=1; break; }
    done
    (( excluded == 0 )) && printf '%s\n' "$r"
  done
  return 0
}
