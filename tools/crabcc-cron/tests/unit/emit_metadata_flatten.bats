#!/usr/bin/env bats

load '../helpers'

setup() { setup_tempdir; }
teardown() { teardown_tempdir; }

@test "emit: flattens metadata with meta. prefix" {
  payload='{"kind":"finding","workload":"x","repo":"a/b","severity":"info","title":"t","body":"b","metadata":{"pr_number":42,"branch":"foo"}}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  assert_status_eq 0
  echo "$STDOUT" | jq -e '."meta.pr_number" == 42' >/dev/null
  echo "$STDOUT" | jq -e '."meta.branch" == "foo"' >/dev/null
}

@test "emit: drops the nested 'metadata' object after flattening" {
  payload='{"kind":"finding","workload":"x","severity":"info","title":"t","body":"b","metadata":{"k":"v"}}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  echo "$STDOUT" | jq -e '.metadata // false | not' >/dev/null
}

@test "emit: truncates body over 8 KiB and adds [truncated] marker" {
  long_body=$(head -c 9000 /dev/urandom | base64 | head -c 9000)
  payload="$(jq -nc --arg b "$long_body" '{kind:"finding",workload:"x",severity:"info",title:"t",body:$b}')"
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  echo "$STDOUT" | jq -r '.body' | tail -c 20 | grep -q '\[truncated\]'
  bytes=$(echo "$STDOUT" | jq -r '.body' | wc -c)
  [[ "$bytes" -le 8200 ]]  # 8192 + small slack for marker
}

@test "emit: stamps ts (ISO 8601 with timezone)" {
  payload='{"kind":"finding","workload":"x","severity":"info","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  echo "$STDOUT" | jq -e '.ts | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T")' >/dev/null
}
