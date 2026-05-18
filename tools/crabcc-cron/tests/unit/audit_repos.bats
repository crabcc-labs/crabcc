#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  # Fake gh on PATH. Routes by first two args.
  mkdir -p "$TMPD/bin"
  cat >"$TMPD/bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
  "repo list")
    cat "$TMPD/fixtures/repo-list.json" 2>/dev/null || echo '[]'
    ;;
  "api "*"/contents/Cargo.toml"*)
    # Path argument is something like /repos/peterlodri-sec/<name>/contents/Cargo.toml
    name=$(echo "$2" | sed 's|.*/peterlodri-sec/\([^/]*\)/.*|\1|')
    if [[ -f "$TMPD/fixtures/cargo-toml-${name}.exists" ]]; then
      echo '{"name":"Cargo.toml"}'
      exit 0
    else
      exit 1
    fi
    ;;
esac
EOF
  chmod +x "$TMPD/bin/gh"
  export PATH="$TMPD/bin:$PATH"
  mkdir -p "$TMPD/fixtures"
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/audit_repos.sh"
}
teardown() { teardown_tempdir; }

@test "audit_repos: returns Rust repos only (those with Cargo.toml)" {
  cat >"$TMPD/fixtures/repo-list.json" <<'EOF'
[{"name":"crabcc","defaultBranch":"main"},{"name":"docs-only","defaultBranch":"main"}]
EOF
  touch "$TMPD/fixtures/cargo-toml-crabcc.exists"
  # docs-only has no Cargo.toml marker
  SECURITY_DENY=()
  result=$(enumerate_audit_repos)
  [[ "$result" == "crabcc" ]]
}

@test "audit_repos: filters out repos in SECURITY_DENY" {
  cat >"$TMPD/fixtures/repo-list.json" <<'EOF'
[{"name":"crabcc","defaultBranch":"main"},{"name":"scratch","defaultBranch":"main"}]
EOF
  touch "$TMPD/fixtures/cargo-toml-crabcc.exists"
  touch "$TMPD/fixtures/cargo-toml-scratch.exists"
  SECURITY_DENY=("scratch")
  result=$(enumerate_audit_repos)
  [[ "$result" == "crabcc" ]]
}

@test "audit_repos: empty gh list returns empty" {
  echo '[]' >"$TMPD/fixtures/repo-list.json"
  SECURITY_DENY=()
  result=$(enumerate_audit_repos)
  [[ -z "$result" ]]
}

@test "audit_repos: all repos denied returns empty" {
  cat >"$TMPD/fixtures/repo-list.json" <<'EOF'
[{"name":"a","defaultBranch":"main"},{"name":"b","defaultBranch":"main"}]
EOF
  touch "$TMPD/fixtures/cargo-toml-a.exists"
  touch "$TMPD/fixtures/cargo-toml-b.exists"
  SECURITY_DENY=("a" "b")
  result=$(enumerate_audit_repos)
  [[ -z "$result" ]]
}
