#!/usr/bin/env bash
# tools/crabcc-cron/tests/helpers.bash — shared bats helpers.

# Always called from the repo root.
CRON_ROOT="${BATS_TEST_DIRNAME}/.."
export CRON_ROOT

# A scratch tempdir per test, auto-cleaned.
setup_tempdir() {
  TMPD="$(mktemp -d)"
  export TMPD
}

teardown_tempdir() {
  rm -rf "$TMPD"
}

# Run a script under test, capture stdout/stderr/status.
# Usage: run_script bin/crabcc-cron-emit --dry-run <<<'{"kind":"finding"...}'
run_script() {
  local script="$1"; shift
  STDOUT="$("${CRON_ROOT}/${script}" "$@" 2>"${TMPD:?}/stderr")"
  STATUS=$?
  STDERR="$(cat "${TMPD}/stderr")"
  export STDOUT STDERR STATUS
}

# Assert STDOUT contains substring.
assert_stdout_contains() {
  [[ "$STDOUT" == *"$1"* ]] || {
    echo "expected STDOUT to contain: $1"
    echo "got: $STDOUT"
    return 1
  }
}

assert_stderr_contains() {
  [[ "$STDERR" == *"$1"* ]] || {
    echo "expected STDERR to contain: $1"
    echo "got: $STDERR"
    return 1
  }
}

assert_status_eq() {
  [[ "$STATUS" -eq "$1" ]] || {
    echo "expected status $1, got $STATUS"
    return 1
  }
}
