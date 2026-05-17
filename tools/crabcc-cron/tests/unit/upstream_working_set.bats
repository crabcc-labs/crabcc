#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/upstream.sh"
}
teardown() { teardown_tempdir; }

@test "upstream: working set = tier2 minus tier3" {
  TIER2_INCLUDE=( "a/b" "c/d" "e/f" )
  TIER3_EXCLUDE=( "c/d" )
  result=$(upstream_working_set)
  [[ "$result" == "a/b"$'\n'"e/f" ]]
}

@test "upstream: empty tier3 leaves tier2 unchanged" {
  TIER2_INCLUDE=( "a/b" "c/d" )
  TIER3_EXCLUDE=()
  result=$(upstream_working_set)
  [[ "$result" == "a/b"$'\n'"c/d" ]]
}

@test "upstream: tier3 covering all of tier2 yields empty set" {
  TIER2_INCLUDE=( "a/b" )
  TIER3_EXCLUDE=( "a/b" )
  result=$(upstream_working_set)
  [[ -z "$result" ]]
}

@test "upstream: empty tier2 yields empty set" {
  TIER2_INCLUDE=()
  TIER3_EXCLUDE=( "a/b" )
  result=$(upstream_working_set)
  [[ -z "$result" ]]
}
