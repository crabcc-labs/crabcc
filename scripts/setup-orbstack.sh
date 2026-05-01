#!/usr/bin/env bash
# setup-orbstack.sh — install + start OrbStack and link it as the
# active Docker context for this repo's container workflow.
#
# OrbStack is the recommended Docker daemon for Apple Silicon Mac per
# apps/CONTAINER-POLICY.md — faster, lighter, and arm64-native vs
# Docker Desktop. Idempotent; safe to re-run.
#
# Usage:
#   bash scripts/setup-orbstack.sh        # install + start + link
#   task setup-orbstack                   # same, via Taskfile entry
#
# Refs: #195

set -euo pipefail

GREEN=$'\033[0;32m'
YELLOW=$'\033[0;33m'
RED=$'\033[0;31m'
BLUE=$'\033[0;34m'
NC=$'\033[0m'

log()  { printf '%s[orbstack-setup]%s %s\n' "$BLUE" "$NC" "$*"; }
ok()   { printf '%s[orbstack-setup]%s ✓ %s\n' "$GREEN" "$NC" "$*"; }
warn() { printf '%s[orbstack-setup]%s ⚠ %s\n' "$YELLOW" "$NC" "$*"; }
err()  { printf '%s[orbstack-setup]%s ✗ %s\n' "$RED" "$NC" "$*" >&2; exit 1; }

# --- 0. host check --------------------------------------------------------

if [[ "$(uname -s)" != "Darwin" ]]; then
    err "OrbStack is macOS-only. On Linux, just use the system Docker daemon."
fi
if [[ "$(uname -m)" != "arm64" ]]; then
    warn "Detected $(uname -m) — OrbStack supports x86_64 too, but our policy is arm64-only."
fi

# --- 1. ensure brew -------------------------------------------------------

if ! command -v brew >/dev/null 2>&1; then
    err "brew required. Install from https://brew.sh first."
fi
ok "brew $(brew --version | head -n1) detected"

# --- 2. install OrbStack (idempotent) -------------------------------------

if [[ -d /Applications/OrbStack.app ]]; then
    ok "OrbStack already installed at /Applications/OrbStack.app"
else
    log "installing OrbStack via brew cask…"
    brew install --cask orbstack
    ok "OrbStack installed"
fi

# --- 3. start OrbStack (if not running) -----------------------------------

if pgrep -x OrbStack >/dev/null 2>&1; then
    ok "OrbStack already running (pid $(pgrep -x OrbStack))"
else
    log "launching OrbStack…"
    open -a OrbStack
fi

# Wait for the docker socket to come up. OrbStack typically takes 2-5 s
# from cold-start. Bounded loop, 30 s ceiling.
log "waiting for docker daemon to become ready…"
for i in $(seq 1 30); do
    if docker info >/dev/null 2>&1; then
        ok "docker daemon ready (after ${i}s)"
        break
    fi
    if [[ $i -eq 30 ]]; then
        err "docker daemon did not respond after 30s. Try opening OrbStack.app manually + re-run this script."
    fi
    sleep 1
done

# --- 4. docker context — switch to orbstack -------------------------------

if ! command -v docker >/dev/null 2>&1; then
    err "docker CLI not on PATH. OrbStack should expose it; try restarting your shell."
fi

CURRENT_CTX="$(docker context show 2>/dev/null || echo unknown)"
if [[ "$CURRENT_CTX" == "orbstack" ]]; then
    ok "docker context already 'orbstack'"
else
    log "switching docker context: $CURRENT_CTX → orbstack"
    if docker context ls --format '{{.Name}}' | grep -q '^orbstack$'; then
        docker context use orbstack
        ok "docker context now 'orbstack'"
    else
        warn "no 'orbstack' context found yet — OrbStack may still be initializing. Re-run in 10 s."
    fi
fi

# --- 5. buildx — ensure a builder exists ----------------------------------
#
# `docker buildx` is what `task docker-build-crabcc` invokes. OrbStack
# ships buildx but no builder is created by default. Create one if
# missing; pick it as current.

if ! docker buildx ls 2>/dev/null | grep -q "default"; then
    warn "no default buildx builder — creating one"
    docker buildx create --use --name crabcc-builder --driver docker-container
    ok "buildx builder 'crabcc-builder' created + selected"
else
    ok "buildx default builder present"
fi

# --- 6. final verification -------------------------------------------------

log "running docker version sanity check"
docker version --format '  Server: {{.Server.Version}} / Client: {{.Client.Version}}' \
    || err "docker version failed"

log "running 'docker run --rm hello-world' (smoke test)"
if docker run --rm hello-world >/dev/null 2>&1; then
    ok "hello-world ran cleanly — OrbStack is working"
else
    warn "hello-world failed; image may already be cached or daemon needs restart"
fi

# --- summary ---------------------------------------------------------------

cat <<EOF

${GREEN}OrbStack setup complete.${NC}

Next steps:
  ${BLUE}task docker-build-crabcc${NC}     # build the crabcc CLI image (#195)
  ${BLUE}task docker-sbom-crabcc${NC}      # generate SPDX SBOM via Syft
  ${BLUE}task docker-push-crabcc${NC}      # push semver tag to ghcr.io

Useful OrbStack commands:
  ${BLUE}orb${NC} status                   # daemon + VM status
  ${BLUE}orb logs${NC}                     # tail OrbStack logs
  open -a OrbStack            # GUI panel (Docker tab + machines list)

Docs: https://docs.orbstack.dev/quick-start
EOF
