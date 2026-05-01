# syntax=docker/dockerfile:1.7
#
# crabcc-agents — BullMQ worker that supervises Claude Code agent
# containers. Runs on the host (or a sibling host) with a Docker socket
# mounted in. Distroless runtime; tini via `--init`.
#
# Build:
#   docker buildx build -f apps/crabcc-agents/Dockerfile \
#     --platform linux/arm64 \
#     -t ghcr.io/peterlodri-sec/crabcc-agents:dev --load \
#     apps/crabcc-agents
#
# Run:
#   docker run -d --name crabcc-agents --restart=unless-stopped --init \
#     -v /var/run/docker.sock:/var/run/docker.sock \
#     -e REDIS_URL=redis://host.docker.internal:6379 \
#     -p 9090:9090 \
#     ghcr.io/peterlodri-sec/crabcc-agents:<semver>

ARG RUST_VERSION=1.90
ARG DEBIAN_VERSION=bookworm

ARG VERSION=dev
ARG REVISION=unknown
ARG CREATED=1970-01-01T00:00:00Z

# --- build stage ----------------------------------------------------------

FROM rust:${RUST_VERSION}-slim-${DEBIAN_VERSION} AS build

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install --no-install-recommends -y \
        ca-certificates \
        clang \
        cmake \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# wild — fast Rust-native linker (https://github.com/wild-linker/wild).
# Cached as its own layer so the wild compile cost (~60-90s) is only
# paid on a wild version bump, not on every code change. The crate's
# `.cargo/config.toml` selects wild for linux-{x86_64,aarch64} via
# `clang -fuse-ld=wild`.
ARG WILD_VERSION=0.6.0
RUN cargo install --locked --version "${WILD_VERSION}" wild-linker

WORKDIR /src

COPY Cargo.toml ./
COPY .cargo/ ./.cargo/
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs
RUN cargo build --release || true

COPY src/ src/
RUN find src -name '*.rs' -exec touch {} +

RUN cargo build --release

# --- runtime stage --------------------------------------------------------

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

ARG VERSION
ARG REVISION
ARG CREATED

LABEL org.opencontainers.image.title="crabcc-agents" \
      org.opencontainers.image.description="BullMQ-driven Claude Code agent runner — sandboxed Docker, Redis Streams logs" \
      org.opencontainers.image.source="https://github.com/peterlodri-sec/crabcc" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.created="${CREATED}" \
      org.opencontainers.image.base.name="gcr.io/distroless/cc-debian12:nonroot"

# Service-side healthcheck — exec'ing /healthz over loopback. Distroless
# has no curl/wget; we ship a tiny static probe binary alongside the
# agent. (Built from src/bin/healthprobe.rs — to be added once a
# dedicated probe is wanted; for now docker-compose `healthcheck:` does
# `curl` from a sidecar or relies on the readiness of the /healthz
# endpoint via host probe.)
EXPOSE 9090

# jemalloc tuning — short-lived per-job allocations dominate, and the
# worker is a long-running dispatcher with bounded memory.
#   narenas:1       — one arena, smallest startup footprint.
#   tcache:true     — per-thread caches (faster small allocs).
#   metadata_thp:auto — transparent huge pages where supported.
#   dirty_decay_ms / muzzy_decay_ms — return memory to the OS faster
#                       (we'd rather give pages back than hoard).
ENV MALLOC_CONF="narenas:1,tcache:true,metadata_thp:auto,dirty_decay_ms:5000,muzzy_decay_ms:5000"

WORKDIR /home/nonroot

COPY --from=build /src/target/release/crabcc-agents /usr/local/bin/crabcc-agents

USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/crabcc-agents"]
