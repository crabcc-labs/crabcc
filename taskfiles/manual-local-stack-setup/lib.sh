#!/usr/bin/env bash
# Shared helpers for taskfiles/manual-local-stack-setup/*.sh
# Each stage script sources this and uses the pass/fail/section printers.

set -uo pipefail

if [[ -t 1 ]]; then
    GRN='\033[32m'; RED='\033[31m'; YEL='\033[33m'; CYN='\033[36m'; BLD='\033[1m'; OFF='\033[0m'
else
    GRN=''; RED=''; YEL=''; CYN=''; BLD=''; OFF=''
fi

_pass=0
_fail=0

section() {
    printf '\n%b== %s ==%b\n' "$BLD$CYN" "$*" "$OFF"
}
pass() {
    _pass=$((_pass + 1))
    printf '  %b✓ PASS%b %s\n' "$GRN" "$OFF" "$*"
}
fail() {
    _fail=$((_fail + 1))
    printf '  %b✗ FAIL%b %s\n' "$RED" "$OFF" "$*"
}
warn() { printf '  %b! WARN%b %s\n' "$YEL" "$OFF" "$*"; }
info() { printf '  %b· %s%b\n' "$CYN" "$*" "$OFF"; }

# Auto-source .env if present so OLLAMA_API_KEY etc are resolvable.
load_env() {
    local f
    for f in install/ollama-stack/.env "$HOME/.crabcc/ollama-stack/.env"; do
        if [[ -f "$f" ]]; then
            set -a; . "$f"; set +a
            return 0
        fi
    done
}

# Standard exit. Each stage script ends with `report`.
report() {
    printf '\n  %d passed, %d failed\n' "$_pass" "$_fail"
    [[ $_fail -eq 0 ]]
}

# Run a command; pass/fail based on exit code. First arg is the
# human description, rest is the command.
must() {
    local desc="$1"; shift
    if "$@" >/dev/null 2>&1; then
        pass "$desc"
    else
        fail "$desc — \`$*\` returned non-zero"
    fi
}

# Same but with an HTTP-status capture: must_http <desc> <expected> <url> [curl-args…]
must_http() {
    local desc="$1"; local expected="$2"; local url="$3"; shift 3
    local code
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "$@" "$url" 2>/dev/null || echo 000)
    if [[ "$code" == "$expected" ]]; then
        pass "$desc (HTTP $code)"
    else
        fail "$desc — expected HTTP $expected, got $code"
    fi
}
