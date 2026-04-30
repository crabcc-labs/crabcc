#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/aliases-smoke.sh
#
# Smoke test for `scripts/install-aliases.sh`. Asserts:
#   1. --print emits the fenced block with required gated aliases.
#   2. --aggressive emits the crabcc-verb aliases (sym/refs/callers/gr).
#   3. Default mode does NOT emit the aggressive verbs (back-compat).
#   4. --aggressive --all-shells --dry-run targets both zsh and bash rc paths.
#   5. The block is idempotent: writing twice into a tempfile keeps one block.
#
# Pure shell — no fixtures, no network. Runs in <1s.
# Used by `task aliases-smoke` and the CI fast-check pre-commit hook.
# ---------------------------------------------------------------------------

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
ALIASES_SH="$SCRIPT_DIR/install-aliases.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ✓ $*"; }

[ -x "$ALIASES_SH" ] || fail "install-aliases.sh not executable: $ALIASES_SH"

echo "→ 1. minimal --print contains gated grep/find/cat aliases"
out="$(bash "$ALIASES_SH" --print --shell zsh 2>&1)"
echo "$out" | grep -q "alias grep='rg'"   || fail "missing grep→rg alias"
echo "$out" | grep -q "alias find='fd'"   || fail "missing find→fd alias"
echo "$out" | grep -q "command -v rg "    || fail "grep alias not gated on command -v"
echo "$out" | grep -q "alias ccs='crabcc sym'" || fail "missing ccs alias"
pass "minimal mode emits gated aliases"

echo "→ 2. minimal mode does NOT emit aggressive verbs (back-compat)"
echo "$out" | grep -q "alias sym=" && fail "aggressive sym alias leaked into minimal mode"
echo "$out" | grep -q "alias refs=" && fail "aggressive refs alias leaked into minimal mode"
pass "minimal mode is back-compat clean"

echo "→ 3. --aggressive emits crabcc verb aliases"
agg="$(bash "$ALIASES_SH" --print --aggressive --shell zsh 2>&1)"
for line in \
    "alias gr='crabcc grep'" \
    "alias sym='crabcc sym'" \
    "alias refs='crabcc refs --files-only'" \
    "alias callers='crabcc callers --files-only'" \
    "alias outline='crabcc outline'" \
    "alias fuzzy='crabcc fuzzy'"
do
    echo "$agg" | grep -qF "$line" || fail "aggressive missing: $line"
done
echo "$agg" | grep -q "command -v delta" || fail "delta not gated"
pass "aggressive emits all crabcc verb aliases"

echo "→ 4. --aggressive --all-shells --dry-run targets zsh + bash"
dry="$(bash "$ALIASES_SH" --aggressive --all-shells --dry-run 2>&1)"
echo "$dry" | grep -q "shell: zsh"  || fail "all-shells missing zsh target"
echo "$dry" | grep -q "shell: bash" || fail "all-shells missing bash target"
echo "$dry" | grep -qE "\.zshrc"     || fail "all-shells missing .zshrc path"
echo "$dry" | grep -qE "\.bashrc|\.bash_profile" || fail "all-shells missing bash rc path"
pass "all-shells targets both zsh + bash"

echo "→ 5. idempotent splice: write twice, only one block remains"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
HOME_BAK="$HOME"
# Redirect HOME so write_block can't touch the real .zshrc.
fake_home="$(mktemp -d)"
trap 'rm -rf "$fake_home"; rm -f "$tmp"' EXIT
HOME="$fake_home" bash "$ALIASES_SH" --aggressive --shell zsh >/dev/null
HOME="$fake_home" bash "$ALIASES_SH" --aggressive --shell zsh >/dev/null
count="$(grep -c '^# >>> crabcc-aliases >>>' "$fake_home/.zshrc")"
[ "$count" = "1" ] || fail "expected 1 fenced block after re-run, got $count"
pass "idempotent: one block after two installs"

echo
echo "✅ aliases-smoke: all assertions passed"
