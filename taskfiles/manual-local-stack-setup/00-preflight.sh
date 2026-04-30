#!/usr/bin/env bash
# Stage 0 — preflight. Verify host has the prerequisites.
# Mirrors §0 of the issue #105 manual-test checklist.
. "$(dirname "$0")/lib.sh"

section "0. Preflight"

if docker --version >/dev/null 2>&1; then
    v=$(docker --version | grep -oE '[0-9]+' | head -1)
    if [[ "${v:-0}" -ge 24 ]]; then pass "docker $v"; else fail "docker too old (need 24+)"; fi
else
    fail "docker not on PATH (or OrbStack not active)"
fi

if docker compose version >/dev/null 2>&1; then
    cv=$(docker compose version --short 2>/dev/null || echo 0.0)
    pass "docker compose $cv"
else
    fail "docker compose plugin missing"
fi

# OrbStack detection on macOS.
if [[ "$(uname -s)" == "Darwin" ]]; then
    if [[ -S "$HOME/.orbstack/run/docker.sock" ]]; then
        info "OrbStack socket present"
    else
        warn "OrbStack not active (Docker Desktop will be used instead)"
    fi
fi

if command -v crabcc >/dev/null 2>&1; then
    pass "crabcc on PATH ($(crabcc --version))"
else
    fail "crabcc not on PATH"
fi

[[ -d ".git" ]] && pass "in repo root" || fail "run this from the crabcc repo root"

report
