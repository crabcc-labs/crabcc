#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  export OSS_FIX_STATE_DIR="$TMPD/state"
  mkdir -p "$OSS_FIX_STATE_DIR"
  # Fake gh that emits a controlled "open PR count" via env.
  mkdir -p "$TMPD/bin"
  cat >"$TMPD/bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
  "pr list")
    # Honor FAKE_OPEN_PR_COUNT
    n="${FAKE_OPEN_PR_COUNT:-0}"
    # Emit n minimal records.
    seq 1 "$n" | jq -nc --argjson n "$n" '[range(0;$n) | {number: (.+1)}]'
    ;;
esac
EOF
  chmod +x "$TMPD/bin/gh"
  export PATH="$TMPD/bin:$PATH"
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/pr.sh"
}
teardown() { teardown_tempdir; }

@test "caps: 0 open PRs → global_cap_ok=0 (under cap)" {
  export FAKE_OPEN_PR_COUNT=0
  run global_cap_reached
  [[ "$status" -ne 0 ]]
}

@test "caps: 2 open PRs → global_cap not reached" {
  export FAKE_OPEN_PR_COUNT=2
  run global_cap_reached
  [[ "$status" -ne 0 ]]
}

@test "caps: 3 open PRs → global_cap reached" {
  export FAKE_OPEN_PR_COUNT=3
  run global_cap_reached
  [[ "$status" -eq 0 ]]
}

@test "caps: per-upstream cap respected when last_pr file is fresh" {
  touch "$OSS_FIX_STATE_DIR/a--b.last_pr"
  run upstream_cap_reached "a/b"
  [[ "$status" -eq 0 ]]
}

@test "caps: per-upstream cap free when last_pr is > 7 days old" {
  touch -t 202604010000 "$OSS_FIX_STATE_DIR/a--b.last_pr"
  run upstream_cap_reached "a/b"
  [[ "$status" -ne 0 ]]
}

@test "caps: per-upstream cap free when no last_pr file" {
  run upstream_cap_reached "fresh/new"
  [[ "$status" -ne 0 ]]
}
