#!/usr/bin/env bats

load '../helpers'

setup() { setup_tempdir; }
teardown() { teardown_tempdir; }

@test "config-shim: emits SECURITY_DENY array from [security_deny] exclude" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[security_deny]
exclude = ["scratch-repo", "old-fork"]
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_status_eq 0
  assert_stdout_contains 'SECURITY_DENY=( "scratch-repo" "old-fork" )'
}

@test "config-shim: missing [security_deny] → empty SECURITY_DENY=()" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[tier2_curated]
include = ["a/b"]
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_stdout_contains 'SECURITY_DENY=()'
}

@test "config-shim: empty [security_deny] exclude list → empty SECURITY_DENY=()" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[security_deny]
exclude = []
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_stdout_contains 'SECURITY_DENY=()'
}

@test "config-shim: all three stanzas emit their arrays" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[tier2_curated]
include = ["a/b"]
[tier3_deny]
exclude = ["c/d"]
[security_deny]
exclude = ["e/f"]
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_stdout_contains 'TIER2_INCLUDE=( "a/b" )'
  assert_stdout_contains 'TIER3_EXCLUDE=( "c/d" )'
  assert_stdout_contains 'SECURITY_DENY=( "e/f" )'
}
