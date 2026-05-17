#!/usr/bin/env bash
# tools/crabcc-cron/tests/e2e/security_smoke.sh
#
# End-to-end smoke for WL-3 security workload.
# Mocks: gh, cargo, crabcc.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd -P)"
TMPD="$(mktemp -d)"
trap 'rm -rf "$TMPD"' EXIT

mkdir -p "$TMPD/bin"

# Fake gh: repo list returns 2 repos; api Cargo.toml exists for both;
# repo clone creates a minimal cargo project under the target dir.
cat >"$TMPD/bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
  "repo list")
    cat <<'JSON'
[{"name":"repo-with-advisory","defaultBranch":"main"},{"name":"repo-clean","defaultBranch":"main"}]
JSON
    ;;
  "api "*"/contents/Cargo.toml"*)
    echo '{"name":"Cargo.toml"}'
    ;;
  "repo clone")
    # The job calls: gh repo clone <owner/name> <dir> -- --quiet
    # → $3="<owner/name>", $4="<dir>", $5="--", $6="--quiet".
    dest="$4"
    mkdir -p "$dest"
    cd "$dest"
    git init --quiet
    git config user.email "bot@example.com"
    git config user.name  "bot"
    cat >Cargo.toml <<'C'
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"
C
    cat >Cargo.lock <<'C'
# Minimal lock; real cargo audit would reject this, but our fake handles it.
version = 3
C
    mkdir -p src; echo 'fn main(){}' >src/main.rs
    git add -A; git commit --quiet -m "init"
    ;;
esac
EOF
chmod +x "$TMPD/bin/gh"

# Fake cargo: audit returns 1 advisory for repo-with-advisory, empty for repo-clean.
# tree returns a 2-line chain. The fake routes by CWD basename.
cat >"$TMPD/bin/cargo" <<'EOF'
#!/usr/bin/env bash
repo_basename="$(basename "$PWD")"
case "$1" in
  audit)
    if [[ "$repo_basename" == "repo-with-advisory" ]]; then
      cat <<'JSON'
{"vulnerabilities":{"list":[
  {"advisory":{"id":"RUSTSEC-2024-0388","title":"use-after-free in hashbrown","description":"UAF","severity":"high","cwe":["CWE-416"]},
   "package":{"name":"hashbrown","version":"0.14.5"},
   "versions":{"patched":[">=0.15.0"]}}
]}}
JSON
    else
      cat <<'JSON'
{"vulnerabilities":{"list":[]}}
JSON
    fi
    exit 0
    ;;
  tree)
    echo "fixture v0.1.0"
    echo "└── hashbrown v0.14.5"
    exit 0
    ;;
esac
EOF
chmod +x "$TMPD/bin/cargo"

# Fake crabcc: index --refresh exits 0, fuzzy returns a few lines.
cat >"$TMPD/bin/crabcc" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  index) exit 0 ;;
  fuzzy)
    # Pretend crate $2 is referenced in 3 files.
    echo "src/main.rs:1: fn main(){}"
    echo "src/foo.rs:2: use foo;"
    echo "src/bar.rs:3: use bar;"
    exit 0
    ;;
esac
EOF
chmod +x "$TMPD/bin/crabcc"

# Build the config.
mkdir -p "$TMPD/etc"
cat >"$TMPD/etc/security.toml" <<'TOML'
[security_deny]
exclude = []
TOML

# Invoke.
PATH="$TMPD/bin:$PATH" \
CRON_ROOT="$ROOT" \
SECURITY_CONFIG="$TMPD/etc/security.toml" \
SECURITY_ROOT="$TMPD/repos" \
bash "$ROOT/jobs/security.sh" > "$TMPD/out.jsonl" 2> "$TMPD/err.log"

echo "--- stdout ---"
cat "$TMPD/out.jsonl"
echo "--- stderr ---"
cat "$TMPD/err.log"

# Assert: exactly 3 findings (1 advisory + 1 clean + 1 summary).
findings=$(grep -c '"kind":"finding"' "$TMPD/out.jsonl" || echo 0)
[[ "$findings" -eq 3 ]] || { echo "FAIL: expected 3 findings, got $findings"; exit 1; }

# Assert: one finding is the RUSTSEC advisory.
grep '"kind":"finding"' "$TMPD/out.jsonl" | jq -e 'select(.metadata.advisory_id == "RUSTSEC-2024-0388") | .severity == "error"' >/dev/null \
  || { echo "FAIL: expected RUSTSEC-2024-0388 finding with severity=error"; exit 1; }

# Assert: one finding is the clean-scan.
grep '"kind":"finding"' "$TMPD/out.jsonl" | jq -e 'select(.title == "security scan clean") | .severity == "info"' >/dev/null \
  || { echo "FAIL: expected security scan clean finding"; exit 1; }

# Assert: one finding is the summary.
grep '"kind":"finding"' "$TMPD/out.jsonl" | jq -e 'select(.title == "security tick complete") | .metadata.repos_scanned == 2' >/dev/null \
  || { echo "FAIL: expected security tick complete finding with repos_scanned=2"; exit 1; }

echo "PASS: security workload end-to-end smoke"
