#!/usr/bin/env bash
# scripts/tests/test-bootstrap.sh — unit tests for scripts/bootstrap.sh.
#
# Exercises:
#   1. shellcheck (when installed) on bootstrap.sh.
#   2. Helpers (mask, read_env_var, file_mode, file_mtime).
#   3. do_show_keys against a mock $HOME / $CRABCC_HOME.
#   4. do_verify against an empty mock $HOME (expect rc=3, all checks fail).
#   5. do_menu rendering (function body inspection — the read loop blocks on
#      /dev/tty so we can't drive it in CI; testing the render is the next-best
#      proxy).
#
# All tests are pure-bash, no bats / no real install side effects.

set -uo pipefail

readonly REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
readonly BOOTSTRAP="$REPO_ROOT/scripts/bootstrap.sh"

c_grn='\033[1;32m'; c_red='\033[1;31m'; c_dim='\033[2m'; c_off='\033[0m'

PASS=0; FAIL=0
TMPDIRS=()
cleanup() { for d in "${TMPDIRS[@]}"; do rm -rf "$d"; done; }
trap cleanup EXIT

mktempdir() { local d; d=$(mktemp -d); TMPDIRS+=("$d"); printf '%s' "$d"; }

assert() {
    local desc="$1"; shift
    if "$@" >/dev/null 2>&1; then
        printf "%b✓%b %s\n" "$c_grn" "$c_off" "$desc"
        PASS=$((PASS+1))
    else
        printf "%b✗%b %s\n" "$c_red" "$c_off" "$desc"
        FAIL=$((FAIL+1))
    fi
}

assert_eq() {
    local desc="$1" expected="$2" actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        printf "%b✓%b %s\n" "$c_grn" "$c_off" "$desc"
        PASS=$((PASS+1))
    else
        printf "%b✗%b %s\n   expected: %q\n   actual:   %q\n" "$c_red" "$c_off" "$desc" "$expected" "$actual"
        FAIL=$((FAIL+1))
    fi
}

assert_contains() {
    local desc="$1" needle="$2" haystack="$3"
    if [[ "$haystack" == *"$needle"* ]]; then
        printf "%b✓%b %s\n" "$c_grn" "$c_off" "$desc"
        PASS=$((PASS+1))
    else
        printf "%b✗%b %s\n   needle: %q\n   actual: %q\n" "$c_red" "$c_off" "$desc" "$needle" "$haystack"
        FAIL=$((FAIL+1))
    fi
}

# === 1. shellcheck =======================================================

if command -v shellcheck >/dev/null 2>&1; then
    if shellcheck -x "$BOOTSTRAP"; then
        printf "%b✓%b shellcheck passes\n" "$c_grn" "$c_off"
        PASS=$((PASS+1))
    else
        printf "%b✗%b shellcheck found issues\n" "$c_red" "$c_off"
        FAIL=$((FAIL+1))
    fi
else
    printf "%b  (shellcheck not installed — skipping that test)%b\n" "$c_dim" "$c_off"
fi

# === 2. Source bootstrap.sh in lib mode ==================================

# shellcheck disable=SC1090
BOOTSTRAP_LIB_ONLY=1 source "$BOOTSTRAP"

assert "log function defined"          test "$(type -t log)"          = "function"
assert "mask function defined"         test "$(type -t mask)"         = "function"
assert "read_env_var function defined" test "$(type -t read_env_var)" = "function"
assert "file_mode function defined"    test "$(type -t file_mode)"    = "function"
assert "do_verify defined"             test "$(type -t do_verify)"    = "function"
assert "do_show_keys defined"          test "$(type -t do_show_keys)" = "function"
assert "do_menu defined"               test "$(type -t do_menu)"      = "function"
assert "main defined"                  test "$(type -t main)"         = "function"

# === 3. mask() ===========================================================

assert_eq "mask: empty"         '<empty>'    "$(mask '')"
assert_eq "mask: 5 chars"       '<5 chars>'  "$(mask 'short')"
assert_eq "mask: 8 chars (boundary)" '<8 chars>' "$(mask '12345678')"
assert_eq "mask: 9 chars"       '1234…6789'  "$(mask '123456789')"
assert_eq "mask: GH OAuth"      'gho_…7sQp'  "$(mask 'gho_aaaaaaaaaaaa7sQp')"

# === 4. read_env_var() ===================================================

ENVFILE=$(mktempdir)/test.env
cat > "$ENVFILE" <<'EOF'
# this is a comment
EMPTY_LINE_NEXT=

TELEGRAM_BOT_TOKEN=8723906293:abcDEF-ghi
QUOTED="quoted-value"
SQUOTED='single-quoted'
LATER=first
LATER=second
EOF

assert_eq "read_env_var: bare value"     '8723906293:abcDEF-ghi'  "$(read_env_var "$ENVFILE" TELEGRAM_BOT_TOKEN)"
assert_eq "read_env_var: dquote-stripped" 'quoted-value'          "$(read_env_var "$ENVFILE" QUOTED)"
assert_eq "read_env_var: squote-stripped" 'single-quoted'         "$(read_env_var "$ENVFILE" SQUOTED)"
assert_eq "read_env_var: last wins"      'second'                 "$(read_env_var "$ENVFILE" LATER)"
assert_eq "read_env_var: missing key"    ''                        "$(read_env_var "$ENVFILE" NO_SUCH_KEY)"
assert    "read_env_var: missing file rc=1" \
    bash -c "BOOTSTRAP_LIB_ONLY=1 source '$BOOTSTRAP'; ! read_env_var /nonexistent/path KEY"

# === 5. do_show_keys with all 4 sources populated =========================

MOCK_HOME=$(mktempdir)
MOCK_CC="$MOCK_HOME/workspace/bin/crabcc"
mkdir -p "$MOCK_CC/install/ollama-stack" \
         "$MOCK_HOME/.claude"

# All 4 sources present:
echo "ollama-secret-key-aaaa1234"        > "$MOCK_HOME/.crabcc.local.api-key"
chmod 0400 "$MOCK_HOME/.crabcc.local.api-key"
echo "TELEGRAM_BOT_TOKEN=12345:abcdef-ghijkl-mnop" > "$MOCK_CC/install/ollama-stack/.env"
echo "LITELLM_MASTER_KEY=sk-master-aaaa-1234"      >> "$MOCK_CC/install/ollama-stack/.env"
cat > "$MOCK_HOME/.claude/settings.local.json" <<'EOF'
{"env": {"GITHUB_PERSONAL_ACCESS_TOKEN": "gho_aaaaaaaaaaaa7sQp"}}
EOF

OUT=$(BS_TEST_HOME="$MOCK_HOME" CRABCC_HOME="$MOCK_CC" do_show_keys 2>&1)

assert_contains "show-keys: ollama key path"       ".crabcc.local.api-key"             "$OUT"
assert_contains "show-keys: ollama key masked"     "olla…1234"                         "$OUT"
assert_contains "show-keys: telegram path"         "install/ollama-stack/.env"         "$OUT"
assert_contains "show-keys: telegram masked"       "1234…mnop"                         "$OUT"
assert_contains "show-keys: litellm path"          "install/ollama-stack/.env"         "$OUT"
assert_contains "show-keys: litellm masked"        "sk-m…1234"                         "$OUT"
if command -v jq >/dev/null 2>&1; then
    assert_contains "show-keys: github PAT path"       ".claude/settings.local.json"   "$OUT"
    assert_contains "show-keys: github PAT masked"     "gho_…7sQp"                     "$OUT"
fi

# === 6. do_show_keys with empty state shows correct hints ================

MOCK_HOME2=$(mktempdir)
MOCK_CC2="$MOCK_HOME2/workspace/bin/crabcc"
mkdir -p "$MOCK_CC2"   # repo dir exists but no keys / .env files

OUT2=$(BS_TEST_HOME="$MOCK_HOME2" CRABCC_HOME="$MOCK_CC2" do_show_keys 2>&1)
assert_contains "show-keys: ollama missing hint"    "Ollama-stack key  not present"   "$OUT2"
assert_contains "show-keys: telegram missing hint"  "Telegram token   not configured" "$OUT2"
assert_contains "show-keys: litellm missing hint"   "LiteLLM master   not present"    "$OUT2"

# === 7. do_verify against empty mock HOME → rc=3 =========================

MOCK_HOME3=$(mktempdir)
set +e
OUT3=$(BS_TEST_HOME="$MOCK_HOME3" CRABCC_HOME="$MOCK_HOME3/workspace/bin/crabcc" IS_MAC=0 do_verify 2>&1)
RC3=$?
set -e

assert_eq "verify: empty home returns rc=3"     "3"               "$RC3"
assert_contains "verify: reports failures"      "verify failed"   "$OUT3"
assert_contains "verify: missing crabcc binary" "crabcc binary present" "$OUT3"

# === 8. do_verify against fully-populated mock → rc=0 ====================

MOCK_HOME4=$(mktempdir)
MOCK_CC4="$MOCK_HOME4/workspace/bin/crabcc"
mkdir -p "$MOCK_HOME4/.cargo/bin"      \
         "$MOCK_HOME4/.claude/skills/crabcc" \
         "$MOCK_HOME4/.claude/commands"  \
         "$MOCK_CC4/.git"
# Stub binaries (just need to be executable files for the test).
echo '#!/bin/sh' > "$MOCK_HOME4/.cargo/bin/crabcc";  chmod +x "$MOCK_HOME4/.cargo/bin/crabcc"
echo '#!/bin/sh' > "$MOCK_HOME4/.cargo/bin/ccc";     chmod +x "$MOCK_HOME4/.cargo/bin/ccc"
ln -s /dev/null "$MOCK_HOME4/.claude/commands/crabcc-init.md"

set +e
OUT4=$(BS_TEST_HOME="$MOCK_HOME4" CRABCC_HOME="$MOCK_CC4" IS_MAC=0 do_verify 2>&1)
RC4=$?
set -e

assert_eq "verify: populated home returns rc=0"  "0"             "$RC4"
assert_contains "verify: passed message"         "verify passed" "$OUT4"

# === 9. do_menu render contains all options ==============================

MENU_BODY=$(declare -f do_menu)
assert_contains "menu: option 1 install everything" "Install everything" "$MENU_BODY"
assert_contains "menu: option 2 CLI only"           "CLI only"           "$MENU_BODY"
assert_contains "menu: option 3 macOS app"          "macOS .app only"    "$MENU_BODY"
assert_contains "menu: option 4 Ollama"             "Ollama stack only"  "$MENU_BODY"
assert_contains "menu: option 5 Telegram"           "Telegram bot only"  "$MENU_BODY"
assert_contains "menu: option 6 show keys"          "Show API keys"      "$MENU_BODY"
assert_contains "menu: option 7 verify"             "Verify"             "$MENU_BODY"
assert_contains "menu: option 8 quit"               "Quit"               "$MENU_BODY"

# === summary =============================================================

echo
if (( FAIL > 0 )); then
    printf "%b✗ %d failed, %d passed%b\n" "$c_red" "$FAIL" "$PASS" "$c_off"
    exit 1
fi
printf "%b✓ all %d tests passed%b\n" "$c_grn" "$PASS" "$c_off"
