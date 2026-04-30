#!/usr/bin/env bash
# Stage 12 — OCI labels on the locally-built crabcc image.
. "$(dirname "$0")/lib.sh"

section "12. Image labels"

if ! command -v task >/dev/null 2>&1; then
    fail "task not on PATH (brew install go-task)"
    report
    exit 1
fi

task images-build >/dev/null 2>&1 \
    && pass "task images-build" || fail "task images-build failed"

# crabcc:local should exist with crabcc-specific labels.
labels=$(docker image inspect crabcc:local --format '{{json .Config.Labels}}' 2>/dev/null)
echo "$labels" | grep -q 'com.crabcc.role'  && pass "com.crabcc.role label set" \
    || fail "com.crabcc.role label missing"
echo "$labels" | grep -q 'com.crabcc.issue' && pass "com.crabcc.issue label set" \
    || fail "com.crabcc.issue label missing"

# Reasonable size.
size_mb=$(docker image inspect crabcc:local --format '{{.Size}}' 2>/dev/null \
        | awk '{ printf("%d", $1/1024/1024) }')
[[ "${size_mb:-0}" -ge 60 && "${size_mb:-0}" -le 200 ]] \
    && pass "image size ${size_mb} MB (in 60-200 range)" \
    || warn "image size ${size_mb} MB (expected 60-200)"

report
