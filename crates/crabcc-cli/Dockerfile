# syntax=docker/dockerfile:1.7
#
# crabcc CLI — multi-stage build, distroless runtime.
# Phase 1 of #195. Policy: apps/CONTAINER-POLICY.md.
#
# Build:
#   docker buildx build -f crates/crabcc-cli/Dockerfile -t crabcc:dev --load .
#   docker run --rm -v $PWD:/work -w /work crabcc:dev index --help
#
# Multi-arch (CI):
#   docker buildx build -f crates/crabcc-cli/Dockerfile \
#     --platform linux/amd64,linux/arm64 \
#     -t ghcr.io/peterlodri-sec/crabcc:$VERSION --push .

ARG RUST_VERSION=1.90
ARG DEBIAN_VERSION=bookworm
#
# Why 1.90 even though workspace MSRV is 1.87: transitive deps move
# faster than our declared MSRV. `time@0.3.47` requires rustc 1.88+
# (per `cargo build --locked` failure 2026-04-30). Pinning the
# Dockerfile a couple of releases above MSRV gives headroom for the
# next transitive bump without churn. Bump again if a future build
# fails on a rustc version constraint.

# Image-metadata args — Taskfile passes the live values; defaults keep
# `docker build` invocations outside the Taskfile self-contained.
ARG VERSION=dev
ARG REVISION=unknown
ARG CREATED=1970-01-01T00:00:00Z

# --- build stage ----------------------------------------------------------
#
# We use the cc-debian12 distroless variant in the runtime stage (allows
# dynamic glibc binaries). The static-debian12 variant would shave ~20 MB
# but requires musl + statically-linking rusqlite/openssl/etc., which is
# fragile across the workspace's transitive deps (per CONTAINER-POLICY.md).
# The cc-debian12 path is the documented "first try" pattern; switch to
# static-debian12 in a follow-up once the dep graph is verified static-
# clean.

FROM rust:${RUST_VERSION}-slim-${DEBIAN_VERSION} AS build

# Build-time deps. build-essential covers cc/ld; cmake + libssl-dev + pkg-config
# cover the bundled-sqlite + bundled-openssl paths.
# clang removed: we use cc (gcc) as the linker driver + wild as the backend.
RUN apt-get update \
    && apt-get install --no-install-recommends -y \
        ca-certificates \
        build-essential \
        cmake \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# CC=cc: consistent C compiler for build scripts.
# wild linker: pure-Rust, ~3-5x faster link than GNU ld on debug + release.
# jemalloc: 5-12% faster on the indexing hot path (alloc-heavy tree-sitter
#   cursors + Vec<Symbol> push). Feature-gated; not(target_env = "msvc").
ENV CC=cc
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=cc
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=wild"
ENV CARGO_INCREMENTAL=0

WORKDIR /src

# Install wild linker. Mounted cargo registry cache keeps this instant on
# rebuilds — the compiled binary lands in /usr/local/cargo/bin/wild which
# is persistent across BuildKit cache runs.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install wild --locked

# --- dependency caching via BuildKit mounts -------------------------------
#
# Mount the cargo registry and the target directory as persistent cache
# volumes. Unlike the COPY-stub trick, BuildKit mounts survive across
# builds transparently — no layer invalidation, no manual touch step.
# Pass `--build-arg BUILDKIT_INLINE_CACHE=1` or use `docker buildx build`
# to activate; falls back to a normal build when BuildKit is unavailable.
COPY Cargo.toml Cargo.lock ./
COPY crates/crabcc-cli/Cargo.toml          crates/crabcc-cli/Cargo.toml
COPY crates/crabcc-core/Cargo.toml         crates/crabcc-core/Cargo.toml
COPY crates/crabcc-mcp/Cargo.toml          crates/crabcc-mcp/Cargo.toml
COPY crates/crabcc-memory/Cargo.toml       crates/crabcc-memory/Cargo.toml
COPY crates/crabcc-memory-bench/Cargo.toml crates/crabcc-memory-bench/Cargo.toml
COPY crates/crabcc-viz/Cargo.toml          crates/crabcc-viz/Cargo.toml

# Stub src so the dep-only compile resolves the full graph cheaply.
RUN set -eux; \
    for crate in crabcc-cli crabcc-core crabcc-mcp crabcc-memory crabcc-memory-bench crabcc-viz; do \
        mkdir -p "crates/$crate/src" "crates/$crate/src/bin"; \
        : > "crates/$crate/src/lib.rs"; \
        echo 'fn main() {}' > "crates/$crate/src/main.rs"; \
    done

# Compile dependencies only. BuildKit cache mounts keep the compiled dep
# artifacts across image rebuilds — no re-download, no re-compile on src change.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --bin crabcc --locked --features crabcc-cli/jemalloc || true

# --- real source ---------------------------------------------------------
COPY crates/ crates/
COPY apps/   apps/
COPY schema/  schema/
COPY install/ install/

# Touch workspace crates to invalidate only our code (deps cache untouched).
RUN find crates -name '*.rs' -exec touch {} +

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --bin crabcc --locked --features crabcc-cli/jemalloc \
 && cp target/release/crabcc /crabcc

# --- runtime stage --------------------------------------------------------
#
# distroless/cc covers the dynamic glibc surface most Rust binaries need
# (bundled sqlite, openssl, etc.). `:nonroot` ships uid 65532; we never
# run as root in production.

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

# Re-import the metadata ARGs so they're available in the runtime stage's
# LABEL expansions (ARG scope is per stage in Docker).
ARG VERSION
ARG REVISION
ARG CREATED

# Full OCI image-spec annotations — discoverable by registries, signed
# by cosign, and consumed by Anchore SBOM action / Snyk / Trivy. See
# apps/CONTAINER-POLICY.md → "Dockerfile conventions" for the canonical
# label set.
LABEL org.opencontainers.image.title="crabcc" \
      org.opencontainers.image.description="Symbol-aware code indexer + agent runtime" \
      org.opencontainers.image.source="https://github.com/peterlodri-sec/crabcc" \
      org.opencontainers.image.url="https://github.com/peterlodri-sec/crabcc" \
      org.opencontainers.image.documentation="https://github.com/peterlodri-sec/crabcc/blob/main/README.md" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.vendor="peterlodri-sec" \
      org.opencontainers.image.authors="Peter Lodri <peter.lodri@instructure.com>" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.created="${CREATED}" \
      org.opencontainers.image.base.name="gcr.io/distroless/cc-debian12:nonroot"

# Working dir is the volume mount point — `docker run -v $PWD:/work`
# pattern. crabcc reads the cwd as the repo to index.
WORKDIR /work

COPY --from=build /crabcc /usr/local/bin/crabcc

USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/crabcc"]
