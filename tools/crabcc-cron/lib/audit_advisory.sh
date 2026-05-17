#!/usr/bin/env bash
# tools/crabcc-cron/lib/audit_advisory.sh
#
# Pure functions for WL-3:
#   map_severity        — cargo-audit severity → crabcc-cron severity
#   advisory_to_finding — assemble a JSONL finding from advisory + context

# Args: cargo_audit_severity (string, possibly empty)
# Echoes: info | warn | error
map_severity() {
  case "${1,,}" in
    critical|high) echo "error" ;;
    medium)        echo "warn"  ;;
    *)             echo "info"  ;;
  esac
}

# Args: repo, advisory_json (one .vulnerabilities.list[] entry),
#       dep_chain (multi-line string), usage_count ("N" or "null")
# Echoes: one JSONL finding line.
advisory_to_finding() {
  local repo="$1" advisory="$2" dep_chain="$3" usage_count="$4"
  local audit_sev crabcc_sev advisory_id crate vuln_version fixed_version
  local title description cwe_first dep_chain_length usage_unavail_reason

  audit_sev="$(jq -r '.advisory.severity // ""' <<<"$advisory")"
  crabcc_sev="$(map_severity "$audit_sev")"
  advisory_id="$(jq -r '.advisory.id // "unknown"' <<<"$advisory")"
  crate="$(jq -r '.package.name // "unknown"' <<<"$advisory")"
  vuln_version="$(jq -r '.package.version // ""' <<<"$advisory")"
  fixed_version="$(jq -r '.versions.patched[0] // ""' <<<"$advisory")"
  description="$(jq -r '.advisory.title // ""' <<<"$advisory")"
  cwe_first="$(jq -r '.advisory.cwe[0] // ""' <<<"$advisory")"

  # Title format: "<advisory_id>: <crate> <<fixed-version> <headline>"
  # fixed_version comes from cargo audit as ">=0.15.0"; strip the ">=" so
  # the title reads "hashbrown <0.15.0 ..." rather than "hashbrown <>=0.15.0 ...".
  local fixed_short="${fixed_version#>=}"
  title="${advisory_id}: ${crate} <${fixed_short} ${description}"

  # Dep chain length: count non-empty lines (0 if "<dep-chain unavailable>")
  if [[ "$dep_chain" == "<dep-chain unavailable>" ]]; then
    dep_chain_length="null"
  else
    dep_chain_length="$(printf '%s\n' "$dep_chain" | grep -cE '^.+' || echo 0)"
  fi

  # Usage count → null vs. number
  if [[ "$usage_count" == "null" ]]; then
    usage_unavail_reason="crabcc index unavailable"
  fi

  # Build body
  local body
  body=$(cat <<EOF
${description}

$(jq -r '.advisory.description // ""' <<<"$advisory")

Vulnerable: ${crate} ${vuln_version}
Fixed in:   ${crate} ${fixed_version}

Reverse-dep chain (from cargo tree --invert):
${dep_chain}

Usage sites in this repo: ${usage_count} files (via crabcc fuzzy ${crate})

Upgrade: cargo update -p ${crate}
EOF
)

  # Build metadata as JSON object
  local meta
  if [[ "$usage_count" == "null" ]]; then
    meta="$(jq -nc \
      --arg id "$advisory_id" \
      --arg crate "$crate" \
      --arg vuln "$vuln_version" \
      --arg fixed "$fixed_version" \
      --arg audit_sev "$audit_sev" \
      --arg dcl "$dep_chain_length" \
      --arg cwe "$cwe_first" \
      --arg reason "$usage_unavail_reason" \
      '{advisory_id:$id, crate:$crate, vulnerable_version:$vuln, fixed_version:$fixed, cargo_audit_severity:$audit_sev, dep_chain_length:(if $dcl == "null" then null else ($dcl|tonumber) end), usage_site_count:null, usage_count_unavailable_reason:$reason, cwe:(if $cwe == "" then null else $cwe end)}')"
  else
    meta="$(jq -nc \
      --arg id "$advisory_id" \
      --arg crate "$crate" \
      --arg vuln "$vuln_version" \
      --arg fixed "$fixed_version" \
      --arg audit_sev "$audit_sev" \
      --arg dcl "$dep_chain_length" \
      --argjson usage "$usage_count" \
      --arg cwe "$cwe_first" \
      '{advisory_id:$id, crate:$crate, vulnerable_version:$vuln, fixed_version:$fixed, cargo_audit_severity:$audit_sev, dep_chain_length:(if $dcl == "null" then null else ($dcl|tonumber) end), usage_site_count:$usage, cwe:(if $cwe == "" then null else $cwe end)}')"
  fi

  # Final finding line
  jq -nc \
    --arg sev "$crabcc_sev" \
    --arg repo "$repo" \
    --arg title "$title" \
    --arg body "$body" \
    --argjson meta "$meta" \
    '{kind:"finding", workload:"security", repo:$repo, severity:$sev, title:$title, body:$body, metadata:$meta}'
}
