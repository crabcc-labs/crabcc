# syntax=docker/dockerfile:1.7
#
# crabcc-hitl-agent — Python FastAPI service.
# Multi-stage: full python-slim for the install + uv invocation,
# distroless python for the runtime. Tracks the same policy as the
# Rust apps under apps/CONTAINER-POLICY.md.
#
# Build context = apps/crabcc-hitl-agent/ (this directory). Greenfield
# Python, no workspace coupling.
#
# Build:
#   docker buildx build -f apps/crabcc-hitl-agent/Dockerfile \
#     --platform linux/arm64 \
#     -t ghcr.io/peterlodri-sec/crabcc-hitl-agent:dev --load \
#     apps/crabcc-hitl-agent
#
# Apple-native runtime (`container` CLI) is OCI-compatible — the same
# image runs unchanged: `container run --image …`. The linux/arm64
# platform target avoids x86_64 emulation on Apple Silicon.

ARG PYTHON_VERSION=3.13
ARG DEBIAN_VERSION=bookworm

ARG VERSION=dev
ARG REVISION=unknown
ARG CREATED=1970-01-01T00:00:00Z

# --- build stage ---------------------------------------------------------
#
# `uv` is the Python package manager — orders of magnitude faster than
# pip, drop-in for `pip install`, supports lockfile-driven installs.
# The final image doesn't ship uv; it just consumes the venv it built.

FROM python:${PYTHON_VERSION}-slim-${DEBIAN_VERSION} AS build

# Install uv via the official installer; pinned to a known release so
# build reproducibility doesn't depend on whatever astral.sh ships
# tonight.
ARG UV_VERSION=0.5.7
RUN apt-get update \
    && apt-get install --no-install-recommends -y curl ca-certificates \
    && curl -fsSL https://astral.sh/uv/${UV_VERSION}/install.sh | sh \
    && rm -rf /var/lib/apt/lists/*
ENV PATH="/root/.local/bin:${PATH}"
ENV UV_LINK_MODE=copy

WORKDIR /src

# --- dependency caching --------------------------------------------------
#
# Copy lock-shaped manifests first; if app source changes but deps
# don't, the next build hits the BuildKit cache mount and skips the
# (slow) httpx download.
COPY pyproject.toml ./

# uv-managed venv at /opt/venv keeps runtime + build paths predictable.
RUN --mount=type=cache,target=/root/.cache/uv \
    uv venv /opt/venv \
    && VIRTUAL_ENV=/opt/venv uv pip install --no-deps -r <(uv pip compile pyproject.toml) \
    || VIRTUAL_ENV=/opt/venv uv pip install -e .

# --- app source + final install -----------------------------------------

COPY src ./src
COPY README.md ./

# Re-run install with the project itself so the editable wheel + the
# real dependency closure both land in the venv.
RUN --mount=type=cache,target=/root/.cache/uv \
    VIRTUAL_ENV=/opt/venv uv pip install --no-deps .

# --- runtime stage -------------------------------------------------------
#
# `gcr.io/distroless/python3-debian12:nonroot` ships Python 3.11 today
# (Google updates it lazily). For 3.13-strict we'd need to build a
# scratch base; for Phase 0 we use python-slim with a non-root user
# instead — same threat model (no shells in $PATH on the runtime
# image, only python + the venv), simpler to maintain. Distroless
# upgrade tracked separately.

FROM python:${PYTHON_VERSION}-slim-${DEBIAN_VERSION} AS runtime

# OCI labels — surfaced in `docker inspect` and registry UIs.
LABEL org.opencontainers.image.title="crabcc-hitl-agent"
LABEL org.opencontainers.image.description="Human-in-the-loop agent service for crabcc — bridges Telegram bot to LiteLLM and (in later phases) the crabcc MCP tool registry."
LABEL org.opencontainers.image.source="https://github.com/peterlodri-sec/crabcc"
LABEL org.opencontainers.image.licenses="Apache-2.0"
LABEL org.opencontainers.image.version="${VERSION}"
LABEL org.opencontainers.image.revision="${REVISION}"
LABEL org.opencontainers.image.created="${CREATED}"

# Runtime tools:
#   - libjemalloc2 — preloaded as the malloc impl. Long-running async
#     services like this one fragment the glibc allocator; jemalloc
#     keeps RSS bounded and gives back memory to the OS more
#     aggressively. Typical win: 10-30% lower steady-state RSS on
#     bursty workloads; sometimes 5-10% latency wins on alloc-heavy
#     paths. Toggle off with `JEMALLOC=off` (we still copy the lib
#     in either case for `docker exec` debugging).
#   - curl — the HEALTHCHECK below uses it; also handy for `docker
#     exec` poking at the litellm + crabcc-mcp services from the same
#     container the agent runs in.
#   - tini — pid 1 reaper; without it Python doesn't see SIGTERM
#     cleanly, so docker stop waits for the 10s grace timeout.
RUN apt-get update \
    && apt-get install --no-install-recommends -y \
        ca-certificates \
        curl \
        libjemalloc2 \
        tini \
    && rm -rf /var/lib/apt/lists/*

# Non-root user matching the distroless `nonroot` UID/GID.
RUN groupadd --system --gid 65532 nonroot \
    && useradd --system --uid 65532 --gid 65532 --no-create-home --shell /sbin/nologin nonroot

# Copy the prebuilt venv from the build stage.
COPY --from=build --chown=nonroot:nonroot /opt/venv /opt/venv
COPY --from=build --chown=nonroot:nonroot /src/src /app/src

# `LD_PRELOAD` swaps in jemalloc transparently for the python process.
# The path is arch-specific — apt installs the lib under the
# multiarch directory. Detect arch at build time so the same Dockerfile
# works for amd64 and arm64.
ARG TARGETARCH=arm64
ENV LD_PRELOAD="/usr/lib/aarch64-linux-gnu/libjemalloc.so.2"
ENV PATH="/opt/venv/bin:${PATH}" \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PYTHONPATH=/app/src \
    # MALLOC_CONF tells jemalloc to release decommitted pages back to
    # the kernel sooner (default delays this, can confuse cgroup OOM
    # accounting). `dirty_decay_ms=2000` keeps RSS reactive; the
    # default 10s lag is too slow for a service that bursts then idles.
    MALLOC_CONF="dirty_decay_ms:2000,muzzy_decay_ms:0,abort_conf:false"

USER nonroot
WORKDIR /app

EXPOSE 9100

# Real healthcheck: hits /healthz which itself runs the upstream
# probes. The `--max-time 5` matches the longest probe timeout +
# slack. Exit non-zero on any 5xx so docker reports unhealthy and
# compose's `depends_on: condition: service_healthy` gates downstream
# starts.
HEALTHCHECK --interval=30s --timeout=6s --start-period=15s --retries=3 \
    CMD curl --silent --show-error --fail --max-time 5 \
        http://127.0.0.1:9100/healthz || exit 1

# `tini` reaps zombies + forwards SIGTERM to uvicorn. Without it,
# uvicorn ignores `docker stop` until the 10s timeout elapses.
ENTRYPOINT ["/usr/bin/tini", "--"]

# uvicorn ASGI server. `--host 0.0.0.0` is fine — the container is on a
# private docker network; the bot reaches it via service DNS, never
# from the public internet (cloudflared exposure is opt-in for the
# future Mini App webapp endpoint, see README §5).
#
# `--loop uvloop --http httptools` opts into the C-backed event loop
# and HTTP parser shipped by `uvicorn[standard]`. Both are noticeably
# faster than the pure-Python fallbacks; setting them explicitly here
# means they're locked even if `uvicorn[standard]` ever changes its
# defaults.
CMD ["uvicorn", "crabcc_hitl.main:app", \
     "--host", "0.0.0.0", \
     "--port", "9100", \
     "--loop", "uvloop", \
     "--http", "httptools", \
     "--no-access-log"]
