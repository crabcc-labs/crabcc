#!/usr/bin/env bash
# Stage 1 — cross-stack bridge network.
. "$(dirname "$0")/lib.sh"

section "1. Shared network"

if [[ -x install/init-shared-network.sh ]]; then
    out=$(install/init-shared-network.sh 2>&1)
    if echo "$out" | grep -qE "(created network crabcc-shared|already exists)"; then
        pass "init-shared-network.sh ran cleanly"
    else
        fail "unexpected output: $out"
    fi
    info_out=$(install/init-shared-network.sh --info 2>&1 | tr -d '\n')
    if echo "$info_out" | grep -q "name=crabcc-shared"; then
        pass "info shows crabcc-shared"
    else
        fail "info missing crabcc-shared: $info_out"
    fi
else
    fail "install/init-shared-network.sh not executable / missing"
fi

report
