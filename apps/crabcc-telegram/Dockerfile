# syntax=docker/dockerfile:1.7
#
# crabcc-telegram bot — multi-stage build, distroless runtime.
# Phase 2 of #195. Policy: apps/CONTAINER-POLICY.md.
#
# Build context = apps/crabcc-telegram/ (this crate is excluded from
# the workspace per the comment in /Cargo.toml — `teloxide` triggers
# a nightly libc ICE under -Z polonius), so the build is standalone.
#
# Build:
#   docker buildx build -f apps/crabcc-telegram/Dockerfile \
#     --platform linux/arm64 \
#     -t ghcr.io/peterlodri-sec/crabcc-telegram:dev --load \
#     apps/crabcc-telegram
#
# Run (typical):
#   docker run -d --name crabcc-telegram --restart=unless-stopped \
#     --env-file apps/crabcc-telegram/.env \
#     ghcr.io/peterlodri-sec/crabcc-telegram:<semver>
#
# The container has NO host filesystem mounts — `/dashboard` works
# (just sends CRABCC_PUBLIC_URL); other bot commands that shell out
# to `crabcc` CLI need a future MCP-over-network refactor (tracked).

ARG RUST_VERSION=1.90
ARG DEBIAN_VERSION=bookworm

ARG VERSION=dev
ARG REVISION=unknown
ARG CREATED=1970-01-01T00:00:00Z

# --- build stage ----------------------------------------------------------

FROM rust:${RUST_VERSION}-slim-${DEBIAN_VERSION} AS build

# pkg-config + libssl-dev cover any reqwest/openssl-sys path the deps
# take. clang for any cc-build-rs based crates (zstd, etc.).
RUN apt-get update \
    && apt-get install --no-install-recommends -y \
        ca-certificates \
        clang \
        cmake \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# --- dependency caching ---------------------------------------------------
#
# COPY only manifest files first; build deps with a stub `main.rs` so
# the heavy dep compilation lands in a cached layer. Real source is
# COPY-ed after.

COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs
RUN cargo build --release --locked || true

# --- real source ---------------------------------------------------------

COPY src/ src/

# Touch every .rs file so cargo recompiles the crate code only (deps
# stay cached from above).
RUN find src -name '*.rs' -exec touch {} +

RUN cargo build --release --locked

# --- runtime stage --------------------------------------------------------

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

ARG VERSION
ARG REVISION
ARG CREATED

LABEL org.opencontainers.image.title="crabcc-telegram" \
      org.opencontainers.image.description="Telegram bot for crabcc — agent runner, status, dashboard Mini App" \
      org.opencontainers.image.source="https://github.com/peterlodri-sec/crabcc" \
      org.opencontainers.image.url="https://github.com/peterlodri-sec/crabcc" \
      org.opencontainers.image.documentation="https://github.com/peterlodri-sec/crabcc/blob/main/apps/crabcc-telegram/README.md" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.vendor="peterlodri-sec" \
      org.opencontainers.image.authors="Peter Lodri <peter.lodri@instructure.com>" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.created="${CREATED}" \
      org.opencontainers.image.base.name="gcr.io/distroless/cc-debian12:nonroot"

# WORKDIR isn't load-bearing — the bot reads .env from process env (via
# --env-file at run time), not from disk. Set to /home/nonroot for
# clarity; distroless's nonroot user owns this path.
WORKDIR /home/nonroot

COPY --from=build /src/target/release/crabcc-telegram /usr/local/bin/crabcc-telegram

USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/crabcc-telegram"]
