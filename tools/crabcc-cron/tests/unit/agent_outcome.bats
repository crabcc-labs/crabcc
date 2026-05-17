#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/agent.sh"
}
teardown() { teardown_tempdir; }

@test "outcome: STATUS=fixed last line → 'fixed'" {
  cat >"$TMPD/log" <<'EOF'
some output
STATUS=fixed
EOF
  result=$(parse_outcome "$TMPD/log" 0)
  [[ "$result" == "fixed" ]]
}

@test "outcome: STATUS=tests-failed → 'tests-failed'" {
  cat >"$TMPD/log" <<'EOF'
STATUS=tests-failed
EOF
  result=$(parse_outcome "$TMPD/log" 0)
  [[ "$result" == "tests-failed" ]]
}

@test "outcome: STATUS=no-fix → 'no-fix'" {
  cat >"$TMPD/log" <<'EOF'
STATUS=no-fix the issue is actually a design discussion
EOF
  result=$(parse_outcome "$TMPD/log" 0)
  [[ "$result" == "no-fix" ]]
}

@test "outcome: SIGTERM exit (124) + no STATUS line → 'timeout'" {
  cat >"$TMPD/log" <<'EOF'
some incomplete work
EOF
  result=$(parse_outcome "$TMPD/log" 124)
  [[ "$result" == "timeout" ]]
}

@test "outcome: non-zero exit + no STATUS line → 'error'" {
  cat >"$TMPD/log" <<'EOF'
crashed
EOF
  result=$(parse_outcome "$TMPD/log" 1)
  [[ "$result" == "error" ]]
}

@test "outcome: zero exit + no STATUS line → 'error'" {
  # Suspicious — agent exited clean without declaring outcome.
  cat >"$TMPD/log" <<'EOF'
quiet exit
EOF
  result=$(parse_outcome "$TMPD/log" 0)
  [[ "$result" == "error" ]]
}
