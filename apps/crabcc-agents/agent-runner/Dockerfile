# syntax=docker/dockerfile:1.7
#
# crabcc-agent-runner — the per-job container the Rust worker spawns.
# Bundles Claude Code CLI + RTK + context-mode plugin so the agent can
# run sandboxed against a tmpfs workspace.
#
# Optimisations:
#   * Alpine base (~7 MB) for low-latency cold start.
#   * tini PID1 reaps zombies even if `docker run --init` is omitted.
#   * `/workspace` and `/tmp` mount as tmpfs at runtime
#     (set by the parent service's HostConfig).
#   * Runtime image has no compilers, no Chrome / Chromium / Playwright,
#     no browser binaries at all. The only shell available is bash
#     (needed for the entrypoint); no GUI toolchain.
#   * SSO auth: the host's `~/.claude/.credentials.json` is bind-mounted
#     read-only into `/home/nonroot/.claude/.credentials.json` by the
#     parent worker. The image pre-creates that directory so the
#     read-only rootfs doesn't block the mount target.
#
# Build:
#   docker buildx build -f apps/crabcc-agents/agent-runner/Dockerfile \
#     --platform linux/arm64 \
#     -t ghcr.io/peterlodri-sec/crabcc-agent-runner:dev --load \
#     apps/crabcc-agents/agent-runner

ARG NODE_VERSION=22
ARG ALPINE_VERSION=3.20

FROM node:${NODE_VERSION}-alpine${ALPINE_VERSION} AS base

ENV CI=1 \
    DEBIAN_FRONTEND=noninteractive \
    NPM_CONFIG_FUND=false \
    NPM_CONFIG_AUDIT=false \
    NPM_CONFIG_UPDATE_NOTIFIER=false \
    CLAUDE_NONINTERACTIVE=1

# tini PID1 reaper (belt-and-braces with `--init`).
# rtk → Rust binary; install via cargo-binstall fallback to `cargo install`.
# context-mode → npm package.
# python3 + py3-pip → required for mini-swe-agent (Python).
RUN apk add --no-cache \
        tini \
        ca-certificates \
        git \
        bash \
        coreutils \
        python3 \
        py3-pip

# Claude Code CLI + axint compiler (Apple-native code-gen + MCP server).
# axint ships an `axint-mcp` stdio bin and an `axint` CLI; both are
# wired into the in-container Claude Code via `mcp.json` (COPY'd below).
# Pinning to a published axint release keeps reproducible builds —
# bump the version on a deliberate cadence rather than tracking latest.
ARG AXINT_VERSION=0.4.16
RUN npm install -g \
        @anthropic-ai/claude-code \
        "@axint/compiler@${AXINT_VERSION}"

# context-mode (https://github.com/mksglu/context-mode) — keeps tool
# output out of the model's context window.
RUN npm install -g context-mode || \
    echo "context-mode npm package not yet published — install from source at runtime"

# RTK (Rust Token Killer — https://github.com/rtk-ai/rtk). Try binary
# release first; fallback to source build under cargo+rust toolchain in a
# scratch builder layer would inflate image size, so we prefer
# cargo-binstall in a builder stage if needed.
RUN wget -qO- https://github.com/rtk-ai/rtk/releases/latest/download/rtk-linux-musl.tar.gz \
      | tar -xz -C /usr/local/bin || \
    echo "rtk binary release not found — rtk install deferred to entrypoint"

# crabcc — code-intelligence CLI + stdio MCP server (sym/refs/callers/outline/
# memory). Gives the in-container agent symbol lookups instead of grep, wired
# into Claude Code via mcp.json. Pinned to a published release. The Linux
# release artifacts are glibc (gnu), so on Alpine/musl we add gcompat; a native
# musl artifact would let us drop the shim (tracked for crabcc release CI).
# Arch-mapped from buildx TARGETARCH. The trailing `crabcc --version` fails the
# build loudly if the download/extract layout is wrong.
ARG CRABCC_VERSION=5.0.0
ARG TARGETARCH
RUN apk add --no-cache gcompat libstdc++ \
 && case "${TARGETARCH:-arm64}" in \
        arm64) _triple=aarch64-unknown-linux-gnu ;; \
        amd64) _triple=x86_64-unknown-linux-gnu ;; \
        *) echo "crabcc: unsupported TARGETARCH=${TARGETARCH}" >&2; exit 1 ;; \
    esac \
 && wget -qO /tmp/crabcc.tgz \
        "https://github.com/crabcc-labs/crabcc/releases/download/v${CRABCC_VERSION}/crabcc-v${CRABCC_VERSION}-${_triple}.tar.gz" \
 && mkdir -p /tmp/cc && tar -xzf /tmp/crabcc.tgz -C /tmp/cc \
 && install -m 0755 "$(find /tmp/cc -type f -name crabcc | head -n1)" /usr/local/bin/crabcc \
 && rm -rf /tmp/cc /tmp/crabcc.tgz \
 && crabcc --version

# mini-swe-agent (https://mini-swe-agent.com) — pinned per release.
# Pinned via build arg so version bumps are deliberate. `--break-
# system-packages` is needed because Alpine ships a PEP 668-managed
# Python; we own this image, so installing into the system site-
# packages is fine. `--no-cache-dir` keeps image size down.
ARG MINI_SWE_VERSION=1.7.0
RUN pip install --break-system-packages --no-cache-dir \
        "mini-swe-agent==${MINI_SWE_VERSION}"

# Non-root user; Claude Code runs as this UID inside the container.
RUN addgroup -S nonroot && adduser -S -G nonroot -u 65532 nonroot

COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 0755 /usr/local/bin/entrypoint.sh

# Static MCP config — Claude Code is launched with `--mcp-config` so
# the read-only rootfs can never be in conflict with first-run approval
# writes, and so the wiring is reproducible per image tag.
COPY mcp.json /etc/crabcc-agent/mcp.json
RUN chmod 0644 /etc/crabcc-agent/mcp.json

# Static mini-swe-agent config. Same rationale as mcp.json — keeps the
# config reproducible per image tag and never collides with read-only
# rootfs.
COPY mini.yaml /etc/crabcc-agent/mini.yaml
RUN chmod 0644 /etc/crabcc-agent/mini.yaml

# /workspace must exist as a mountpoint for the tmpfs.
RUN mkdir -p /workspace && chown nonroot:nonroot /workspace

# /home/nonroot/.claude/ is the bind-mount target for the host's SSO
# credentials. Pre-created (with the right ownership) so the read-only
# rootfs doesn't block bollard from mounting onto it.
RUN mkdir -p /home/nonroot/.claude && chown -R nonroot:nonroot /home/nonroot/.claude

USER nonroot:nonroot
WORKDIR /workspace

# HEALTHCHECK exists so Docker reports per-container health even when
# `docker run --health-*` flags aren't passed. Trivial: succeed if
# `claude` is on PATH.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=2 \
    CMD claude --version > /dev/null 2>&1 || exit 1

ENTRYPOINT ["/sbin/tini", "--", "/usr/local/bin/entrypoint.sh"]
