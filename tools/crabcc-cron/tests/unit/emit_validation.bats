#!/usr/bin/env bats

load '../helpers'

setup() { setup_tempdir; }
teardown() { teardown_tempdir; }

@test "emit: ignores log lines, only emits findings" {
  run_script bin/crabcc-cron-emit --dry-run <<<'{"kind":"log","level":"info","msg":"hi"}'
  assert_status_eq 0
  [[ -z "$STDOUT" ]] || { echo "expected empty stdout"; return 1; }
}

@test "emit: rejects finding missing required field 'workload'" {
  run_script bin/crabcc-cron-emit --dry-run <<<'{"kind":"finding","severity":"info","title":"t","body":"b"}'
  assert_status_eq 2
  assert_stderr_contains "missing required field: workload"
}

@test "emit: rejects finding with unknown severity" {
  payload='{"kind":"finding","workload":"x","severity":"meh","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  assert_status_eq 2
  assert_stderr_contains "invalid severity"
}

@test "emit: ignores non-JSON lines with a warning" {
  run_script bin/crabcc-cron-emit --dry-run <<<'not-json'
  assert_status_eq 0
  assert_stderr_contains "skipped non-JSON line"
}

@test "emit: ignores unknown 'kind' values" {
  run_script bin/crabcc-cron-emit --dry-run <<<'{"kind":"unknown","msg":"x"}'
  assert_status_eq 0
}
