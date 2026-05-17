#!/usr/bin/env bash
# tools/crabcc-cron/tests/e2e/oss_fix_dryrun.sh
#
# End-to-end smoke for the OSS-fix dispatcher in DRY_RUN mode.
# Mocks: gh (returns canned issue), opencode (writes STATUS=fixed),
#        git+jq+date+curl+python3 are real.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd -P)"
TMPD="$(mktemp -d)"
trap 'rm -rf "$TMPD"' EXIT

# Build the fake bin dir.
mkdir -p "$TMPD/bin"

cat >"$TMPD/bin/gh" <<'EOF'
#!/usr/bin/env bash
# Fake gh. Handles: issue list, repo view, repo clone, pr list, pr create.
case "$1 $2" in
  "issue list")
    cat <<'JSON'
[{"number":42,"title":"trivial fix","body":"do the thing","createdAt":"2026-04-01T00:00:00Z","updatedAt":"2026-04-01T00:00:00Z","assignees":[],"linkedBranches":[],"comments":{"totalCount":1}}]
JSON
    ;;
  "repo view") echo '{"stargazerCount":1000}' ;;
  "repo clone")
    # Args layout: $1=repo, $2=clone, $3=<owner/name>, $4=<dest_dir>, $5=--, $6+=passthrough
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
    mkdir -p src; echo 'fn main(){}' >src/main.rs
    git add -A; git commit --quiet -m "init"
    ;;
  "pr list")  echo '[]' ;;
  "pr create") echo "https://github.com/example/repo/pull/9999" ;;
esac
EOF
chmod +x "$TMPD/bin/gh"

cat >"$TMPD/bin/opencode" <<'EOF'
#!/usr/bin/env bash
# opencode mock: writes STATUS=fixed and exits 0.
echo "STATUS=fixed"
exit 0
EOF
chmod +x "$TMPD/bin/opencode"

# Build the config.
mkdir -p "$TMPD/etc"
cat >"$TMPD/etc/oss-fix.toml" <<'TOML'
[tier2_curated]
include = ["example/repo"]
[tier3_deny]
exclude = []
TOML

# Build the state dir.
mkdir -p "$TMPD/state"

# Invoke.
PATH="$TMPD/bin:$PATH" \
CRON_ROOT="$ROOT" \
OSS_FIX_CONFIG="$TMPD/etc/oss-fix.toml" \
OSS_FIX_STATE_DIR="$TMPD/state" \
OSS_FIX_DRY_RUN=1 \
SANDBOX_ROOT="$TMPD/sandbox" \
NOW="2026-05-17T00:00:00Z" \
bash "$ROOT/jobs/oss-fix.sh" > "$TMPD/out.jsonl" 2> "$TMPD/err.log"

echo "--- stdout ---"
cat "$TMPD/out.jsonl"
echo "--- stderr ---"
cat "$TMPD/err.log"

# Assert exactly one finding and it has status=pr_opened_dryrun.
findings=$(grep -c '"kind":"finding"' "$TMPD/out.jsonl" || echo 0)
[[ "$findings" -eq 1 ]] || { echo "FAIL: expected 1 finding, got $findings"; exit 1; }
grep '"kind":"finding"' "$TMPD/out.jsonl" | jq -e '.title == "pr_opened_dryrun"' >/dev/null || {
  echo "FAIL: expected title=pr_opened_dryrun"; exit 1;
}

echo "PASS: oss-fix dispatcher end-to-end dry-run"
