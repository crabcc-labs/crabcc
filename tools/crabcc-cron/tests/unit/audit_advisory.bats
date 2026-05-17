#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/audit_advisory.sh"
}
teardown() { teardown_tempdir; }

# --- map_severity ---

@test "severity: critical → error" {
  result=$(map_severity "critical")
  [[ "$result" == "error" ]]
}

@test "severity: high → error" {
  result=$(map_severity "high")
  [[ "$result" == "error" ]]
}

@test "severity: medium → warn" {
  result=$(map_severity "medium")
  [[ "$result" == "warn" ]]
}

@test "severity: low → info" {
  result=$(map_severity "low")
  [[ "$result" == "info" ]]
}

@test "severity: informational → info" {
  result=$(map_severity "informational")
  [[ "$result" == "info" ]]
}

@test "severity: empty/missing → info" {
  result=$(map_severity "")
  [[ "$result" == "info" ]]
}

@test "severity: unknown value → info (safe default)" {
  result=$(map_severity "moderate-by-someone-else")
  [[ "$result" == "info" ]]
}

# --- advisory_to_finding ---

@test "advisory_to_finding: full happy path with dep chain and usage count" {
  advisory='{
    "advisory": {
      "id": "RUSTSEC-2024-0388",
      "title": "use-after-free in hashbrown",
      "description": "Affected versions of this crate are vulnerable to UAF.",
      "severity": "high",
      "cwe": ["CWE-416"]
    },
    "package": {
      "name": "hashbrown",
      "version": "0.14.5"
    },
    "versions": {
      "patched": [">=0.15.0"]
    }
  }'
  dep_chain="hashbrown 0.14.5\n└── indexmap 2.2.6"
  result=$(advisory_to_finding "crabcc" "$advisory" "$dep_chain" "12")
  # Validate it is a single-line JSON.
  echo "$result" | jq -e . >/dev/null
  echo "$result" | jq -e '.kind == "finding"' >/dev/null
  echo "$result" | jq -e '.workload == "security"' >/dev/null
  echo "$result" | jq -e '.repo == "crabcc"' >/dev/null
  echo "$result" | jq -e '.severity == "error"' >/dev/null
  echo "$result" | jq -e '.title | test("^RUSTSEC-2024-0388")' >/dev/null
  echo "$result" | jq -e '.metadata.advisory_id == "RUSTSEC-2024-0388"' >/dev/null
  echo "$result" | jq -e '.metadata.crate == "hashbrown"' >/dev/null
  echo "$result" | jq -e '.metadata.usage_site_count == 12' >/dev/null
  echo "$result" | jq -e '.metadata.cwe == "CWE-416"' >/dev/null
}

@test "advisory_to_finding: null usage_count produces metadata.usage_site_count == null" {
  advisory='{
    "advisory": {"id":"RUSTSEC-X","title":"t","description":"d","severity":"medium","cwe":[]},
    "package": {"name":"foo","version":"1.0.0"},
    "versions": {"patched":[">=2.0.0"]}
  }'
  result=$(advisory_to_finding "crabcc" "$advisory" "foo 1.0.0" "null")
  echo "$result" | jq -e '.metadata.usage_site_count == null' >/dev/null
  echo "$result" | jq -e '.metadata.usage_count_unavailable_reason | type == "string"' >/dev/null
}

@test "advisory_to_finding: empty cwe array → metadata.cwe == null" {
  advisory='{
    "advisory": {"id":"RUSTSEC-X","title":"t","description":"d","severity":"low","cwe":[]},
    "package": {"name":"foo","version":"1.0.0"},
    "versions": {"patched":[">=2.0.0"]}
  }'
  result=$(advisory_to_finding "crabcc" "$advisory" "foo 1.0.0" "0")
  echo "$result" | jq -e '.metadata.cwe == null' >/dev/null
}

@test "advisory_to_finding: severity mapped through map_severity" {
  advisory='{
    "advisory": {"id":"RUSTSEC-X","title":"t","description":"d","severity":"critical","cwe":[]},
    "package": {"name":"foo","version":"1.0.0"},
    "versions": {"patched":[">=2.0.0"]}
  }'
  result=$(advisory_to_finding "crabcc" "$advisory" "foo 1.0.0" "5")
  echo "$result" | jq -e '.severity == "error"' >/dev/null
}
