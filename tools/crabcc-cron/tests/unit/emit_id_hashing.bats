#!/usr/bin/env bats

load '../helpers'

setup() { setup_tempdir; }
teardown() { teardown_tempdir; }

@test "emit: computes id from workload+repo+title when id absent" {
  payload='{"kind":"finding","workload":"oss-fix","repo":"a/b","severity":"info","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  assert_status_eq 0
  # Dry-run prints the would-be POST body. Assert id field is a 64-char hex string.
  echo "$STDOUT" | jq -e '.id | test("^[a-f0-9]{64}$")' >/dev/null
}

@test "emit: preserves caller-supplied id" {
  payload='{"kind":"finding","id":"my-explicit-id","workload":"oss-fix","repo":"a/b","severity":"info","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  echo "$STDOUT" | jq -e '.id == "my-explicit-id"' >/dev/null
}

@test "emit: same input → same id (deterministic)" {
  payload='{"kind":"finding","workload":"oss-fix","repo":"a/b","severity":"info","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  ID1="$(echo "$STDOUT" | jq -r '.id')"
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  ID2="$(echo "$STDOUT" | jq -r '.id')"
  [[ "$ID1" == "$ID2" ]] || { echo "ids differ: $ID1 vs $ID2"; return 1; }
}
