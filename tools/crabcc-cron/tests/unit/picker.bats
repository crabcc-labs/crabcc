#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  export NOW="2026-05-17T00:00:00Z"
  # Fake gh: returns fixture-based JSON from $TMPD/fixtures.
  mkdir -p "$TMPD/bin"
  cat >"$TMPD/bin/gh" <<'EOF'
#!/usr/bin/env bash
# Args: issue list --repo <r> --label ... --state open --json ...
# OR:   repo view <r> --json stargazerCount
# Routes based on first two args.
case "$1 $2" in
  "issue list")
    # find --repo flag value
    while [[ $# -gt 0 ]]; do
      case "$1" in --repo) shift; repo="$1" ;; esac
      shift
    done
    cat "$TMPD/fixtures/issues-${repo//\//--}.json" 2>/dev/null || echo "[]"
    ;;
  "repo view")
    repo="$3"
    cat "$TMPD/fixtures/repo-${repo//\//--}.json" 2>/dev/null || echo '{"stargazerCount":0}'
    ;;
esac
EOF
  chmod +x "$TMPD/bin/gh"
  export PATH="$TMPD/bin:$PATH"
  mkdir -p "$TMPD/fixtures"
  export OSS_FIX_STATE_DIR="$TMPD/state"
  mkdir -p "$OSS_FIX_STATE_DIR"

  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/eligibility.sh"
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/upstream.sh"
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/picker.sh"
}
teardown() { teardown_tempdir; }

@test "picker: returns null when no upstreams" {
  result=$(pick_issue)
  [[ "$result" == "" ]]
}

@test "picker: returns null when all upstreams have no eligible issues" {
  echo '[]' >"$TMPD/fixtures/issues-a--b.json"
  echo '{"stargazerCount":100}' >"$TMPD/fixtures/repo-a--b.json"
  TIER2_INCLUDE=( "a/b" )
  TIER3_EXCLUDE=()
  result=$(pick_issue)
  [[ "$result" == "" ]]
}

@test "picker: returns the eligible issue from the only upstream" {
  cat >"$TMPD/fixtures/issues-a--b.json" <<EOF
[{"number":42,"title":"t","createdAt":"2026-04-01T00:00:00Z","updatedAt":"2026-04-01T00:00:00Z","assignees":[],"linkedBranches":[],"comments":{"totalCount":3}}]
EOF
  echo '{"stargazerCount":50}' >"$TMPD/fixtures/repo-a--b.json"
  TIER2_INCLUDE=( "a/b" )
  TIER3_EXCLUDE=()
  result=$(pick_issue)
  echo "$result" | jq -e '.repo == "a/b"' >/dev/null
  echo "$result" | jq -e '.issue.number == 42' >/dev/null
}

@test "picker: prefers upstream with higher star count" {
  cat >"$TMPD/fixtures/issues-a--b.json" <<EOF
[{"number":10,"title":"low","createdAt":"2026-04-01T00:00:00Z","updatedAt":"2026-04-01T00:00:00Z","assignees":[],"linkedBranches":[],"comments":{"totalCount":1}}]
EOF
  echo '{"stargazerCount":10}' >"$TMPD/fixtures/repo-a--b.json"
  cat >"$TMPD/fixtures/issues-c--d.json" <<EOF
[{"number":99,"title":"high","createdAt":"2026-04-01T00:00:00Z","updatedAt":"2026-04-01T00:00:00Z","assignees":[],"linkedBranches":[],"comments":{"totalCount":1}}]
EOF
  echo '{"stargazerCount":1000}' >"$TMPD/fixtures/repo-c--d.json"
  TIER2_INCLUDE=( "a/b" "c/d" )
  TIER3_EXCLUDE=()
  result=$(pick_issue)
  echo "$result" | jq -e '.repo == "c/d"' >/dev/null
  echo "$result" | jq -e '.issue.number == 99' >/dev/null
}

@test "picker: skips issues already attempted (state file present)" {
  cat >"$TMPD/fixtures/issues-a--b.json" <<EOF
[{"number":42,"title":"t","createdAt":"2026-04-01T00:00:00Z","updatedAt":"2026-04-01T00:00:00Z","assignees":[],"linkedBranches":[],"comments":{"totalCount":3}}]
EOF
  echo '{"stargazerCount":50}' >"$TMPD/fixtures/repo-a--b.json"
  touch "$OSS_FIX_STATE_DIR/a--b--42.attempted"
  TIER2_INCLUDE=( "a/b" )
  TIER3_EXCLUDE=()
  result=$(pick_issue)
  [[ "$result" == "" ]]
}
