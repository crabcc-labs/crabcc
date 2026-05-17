#!/usr/bin/env bats

load '../helpers'

setup() { setup_tempdir; }
teardown() { teardown_tempdir; }

@test "config-shim: emits shell var for tier2 include list" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[tier2_curated]
include = ["a/b", "c/d"]

[tier3_deny]
exclude = []
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_status_eq 0
  assert_stdout_contains 'TIER2_INCLUDE=( "a/b" "c/d" )'
}

@test "config-shim: emits shell var for tier3 deny list" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[tier2_curated]
include = ["a/b"]

[tier3_deny]
exclude = ["x/y", "z/w"]
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_stdout_contains 'TIER3_EXCLUDE=( "x/y" "z/w" )'
}

@test "config-shim: missing file → exit 2 with clear error" {
  run_script bin/crabcc-cron-config-shim "$TMPD/nonexistent.toml"
  assert_status_eq 2
  assert_stderr_contains "config file not found"
}

@test "config-shim: missing tier2_curated section → empty include" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[tier3_deny]
exclude = []
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_stdout_contains 'TIER2_INCLUDE=()'
}
