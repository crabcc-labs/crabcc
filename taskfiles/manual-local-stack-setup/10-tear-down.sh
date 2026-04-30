#!/usr/bin/env bash
# Stage 10 — tear-down. Soft + hard reset.
. "$(dirname "$0")/lib.sh"

section "10. Tear-down"

crabcc ollama-stack down >/dev/null 2>&1 \
    && pass "soft down" || fail "soft down failed"

ps_n=$(docker compose -f install/ollama-stack/docker-compose.yml ps -q | wc -l | tr -d ' ')
[[ "$ps_n" == "0" ]] && pass "no running containers" || fail "$ps_n containers still up"

# Volumes preserved by default.
vols=$(docker volume ls --format '{{.Name}}' | grep -c crabcc-ollama || echo 0)
[[ "$vols" -ge 1 ]] && pass "$vols ollama-stack volume(s) preserved" \
    || warn "no ollama-stack volumes (acceptable on first run)"

# Hard reset — wipe volumes.
crabcc ollama-stack down --volumes >/dev/null 2>&1 \
    && pass "down --volumes" || fail "down --volumes failed"

vols_after=$(docker volume ls --format '{{.Name}}' | grep -c crabcc-ollama || echo 0)
[[ "$vols_after" == "0" ]] && pass "volumes wiped" \
    || warn "$vols_after volumes still present"

# Remove the cross-stack bridge.
if [[ -x install/init-shared-network.sh ]]; then
    install/init-shared-network.sh --rm >/dev/null 2>&1 \
        && pass "shared-network --rm" || fail "shared-network --rm failed"
fi

report
