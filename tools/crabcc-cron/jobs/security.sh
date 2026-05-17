#!/usr/bin/env bash
# tools/crabcc-cron/jobs/security.sh — WL-3 entrypoint.
#
# Daily 02:00 UTC: walk every peterlodri-sec Rust repo, cargo audit,
# emit one finding per advisory + one clean-scan finding per clean repo
# + one summary finding.

set -uo pipefail

CRON_ROOT="${CRON_ROOT:-/opt/crabcc-cron}"

# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/log.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/audit_repos.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/audit_advisory.sh"

: "${SECURITY_CONFIG:=/etc/crabcc-cron/security.toml}"
: "${SECURITY_ROOT:=/srv/cron-agents/security}"
mkdir -p "$SECURITY_ROOT"

# Defensive init: ensures audit_repos never trips set -u
# even if the config shim fails to emit the SECURITY_DENY array.
# shellcheck disable=SC2034  # consumed by sourced lib/audit_repos.sh
SECURITY_DENY=()

eval "$("${CRON_ROOT}/bin/crabcc-cron-config-shim" "$SECURITY_CONFIG")"

t_start=$(date +%s)
log_info "security tick start"

repos_scanned=0
repos_clean=0
advisories_total=0

while IFS= read -r repo; do
  [[ -z "$repo" ]] && continue
  repos_scanned=$((repos_scanned + 1))
  log_info "scanning $repo"

  dir="${SECURITY_ROOT}/${repo}"
  if [[ -d "$dir/.git" ]]; then
    if ! git -C "$dir" pull --quiet 2>/dev/null; then
      log_warn "$repo: git pull failed, re-cloning"
      rm -rf "$dir"
      if ! gh repo clone "peterlodri-sec/${repo}" "$dir" -- --quiet 2>/dev/null; then
        emit_finding error security "peterlodri-sec/$repo" "clone failed" \
          "Failed to clone or pull peterlodri-sec/${repo}." '{}'
        continue
      fi
    fi
  else
    if ! gh repo clone "peterlodri-sec/${repo}" "$dir" -- --quiet 2>/dev/null; then
      emit_finding error security "peterlodri-sec/$repo" "clone failed" \
        "Failed to clone peterlodri-sec/${repo}." '{}'
      continue
    fi
  fi

  # Skip if no Cargo.lock — cargo audit needs it.
  if [[ ! -f "$dir/Cargo.lock" ]]; then
    emit_finding info security "peterlodri-sec/$repo" "skipped (no Cargo.lock)" \
      "Repo has Cargo.toml but no Cargo.lock; cargo audit needs the lockfile." '{}'
    continue
  fi

  # Refresh crabcc index (best effort; failures only impact usage counts).
  index_available=1
  if ! (cd "$dir" && crabcc index --refresh >/dev/null 2>&1); then
    log_warn "$repo: crabcc index --refresh failed; usage counts disabled"
    index_available=0
  fi

  # Run cargo audit — non-zero exit is normal when advisories present.
  audit_json="$(cd "$dir" && cargo audit --json 2>/dev/null || true)"
  if [[ -z "$audit_json" ]]; then
    emit_finding error security "peterlodri-sec/$repo" "cargo-audit failed" \
      "cargo audit produced empty output for ${repo}." '{}'
    continue
  fi
  if ! jq -e . <<<"$audit_json" >/dev/null 2>&1; then
    emit_finding error security "peterlodri-sec/$repo" "cargo-audit failed" \
      "cargo audit produced malformed JSON for ${repo}." '{}'
    continue
  fi

  # Walk advisories.
  advisories_in_repo=0
  while IFS= read -r advisory; do
    [[ -z "$advisory" ]] && continue
    advisories_in_repo=$((advisories_in_repo + 1))

    crate="$(jq -r '.package.name' <<<"$advisory")"

    # Dep chain via cargo tree --invert.
    dep_chain="$(cd "$dir" && cargo tree --invert -p "$crate" --no-default-features 2>/dev/null || echo '<dep-chain unavailable>')"
    [[ -z "$dep_chain" ]] && dep_chain="<dep-chain unavailable>"

    # Usage count via crabcc fuzzy.
    if (( index_available == 1 )); then
      usage_count="$(cd "$dir" && crabcc fuzzy "$crate" 2>/dev/null | wc -l | tr -d ' ' || echo 'null')"
      [[ -z "$usage_count" ]] && usage_count="null"
    else
      usage_count="null"
    fi

    advisory_to_finding "peterlodri-sec/$repo" "$advisory" "$dep_chain" "$usage_count"
  done < <(jq -c '.vulnerabilities.list[]?' <<<"$audit_json")

  advisories_total=$((advisories_total + advisories_in_repo))

  if (( advisories_in_repo == 0 )); then
    repos_clean=$((repos_clean + 1))
    direct_deps="$(cd "$dir" && cargo tree --depth 1 2>/dev/null | wc -l | tr -d ' ' || echo 0)"
    transitive_deps="$(cd "$dir" && cargo tree 2>/dev/null | wc -l | tr -d ' ' || echo 0)"
    emit_finding info security "peterlodri-sec/$repo" "security scan clean" \
      "cargo audit found no advisories. ${direct_deps} direct deps, ${transitive_deps} transitive." \
      "$(jq -nc --argjson dd "$direct_deps" --argjson td "$transitive_deps" \
          '{direct_deps:$dd, transitive_deps:$td}')"
  fi
done < <(enumerate_audit_repos)

duration=$(( $(date +%s) - t_start ))

emit_finding info security "" "security tick complete" \
  "Scanned $repos_scanned repos, $advisories_total advisories, $repos_clean repos clean. Duration: ${duration}s." \
  "$(jq -nc \
      --argjson scanned "$repos_scanned" \
      --argjson advisories "$advisories_total" \
      --argjson clean "$repos_clean" \
      --argjson dur "$duration" \
      '{repos_scanned:$scanned, advisories_total:$advisories, repos_clean:$clean, duration_s:$dur}')"

log_info "security tick done in ${duration}s"
exit 0
