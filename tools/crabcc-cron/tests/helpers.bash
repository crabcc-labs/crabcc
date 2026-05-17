#!/usr/bin/env bash
# tools/crabcc-cron/tests/helpers.bash — shared bats helpers.

# CRON_ROOT is the tool root (where bin/ lives). Anchor on this helpers
# file's location so tests can be nested arbitrarily under tests/.
CRON_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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
  # Disable errexit so a non-zero exit code from the script under test
  # doesn't abort the surrounding bats test (bats enables `set -e` in
  # @test blocks). We re-enable it afterwards.
  set +e
  STDOUT="$("${CRON_ROOT}/${script}" "$@" 2>"${TMPD:?}/stderr")"
  STATUS=$?
  set -e
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
