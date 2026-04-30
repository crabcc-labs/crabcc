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

ARG RUST_VERSION=1.87
ARG DEBIAN_VERSION=bookworm

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

# Build-time deps. cmake / clang / libssl-dev cover the bundled-sqlite +
# bundled-openssl paths some workspace crates take. Pinned to the
# Debian-stable repo to keep the manifest reproducible.
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
# COPY only manifest files first so dependency compilation lands in its
# own cacheable layer. Source changes don't invalidate the deps layer.
COPY Cargo.toml Cargo.lock ./
COPY crates/crabcc-cli/Cargo.toml          crates/crabcc-cli/Cargo.toml
COPY crates/crabcc-core/Cargo.toml         crates/crabcc-core/Cargo.toml
COPY crates/crabcc-mcp/Cargo.toml          crates/crabcc-mcp/Cargo.toml
COPY crates/crabcc-memory/Cargo.toml       crates/crabcc-memory/Cargo.toml
COPY crates/crabcc-memory-bench/Cargo.toml crates/crabcc-memory-bench/Cargo.toml
COPY crates/crabcc-viz/Cargo.toml          crates/crabcc-viz/Cargo.toml

# Stub out src/ for every workspace member so `cargo build` resolves the
# dep graph without compiling our actual code yet. This is the standard
# Rust Docker cache trick.
RUN set -eux; \
    for crate in crabcc-cli crabcc-core crabcc-mcp crabcc-memory crabcc-memory-bench crabcc-viz; do \
        mkdir -p "crates/$crate/src" "crates/$crate/src/bin"; \
        : > "crates/$crate/src/lib.rs"; \
        echo 'fn main() {}' > "crates/$crate/src/main.rs"; \
    done

# Build dependencies only (the stub src compiles trivially; heavyweight
# work is in the dep graph). `--locked` enforces the committed lockfile.
RUN cargo build --release --bin crabcc --locked || true

# --- real source ---------------------------------------------------------
#
# Now COPY the actual source. The previous deps layer stays cached unless
# Cargo.lock or one of the manifest files changed.
COPY crates/ crates/
COPY apps/   apps/

# Touch every workspace member's src to force re-build of the workspace
# crates only (deps stay cached from above). Without this, `cargo`
# sometimes thinks the stub src is fresher than the real one.
RUN find crates -name '*.rs' -exec touch {} +

RUN cargo build --release --bin crabcc --locked

# --- runtime stage --------------------------------------------------------
#
# distroless/cc covers the dynamic glibc surface most Rust binaries need
# (bundled sqlite, openssl, etc.). `:nonroot` ships uid 65532; we never
# run as root in production.

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

LABEL org.opencontainers.image.source="https://github.com/peterlodri-sec/crabcc"
LABEL org.opencontainers.image.title="crabcc"
LABEL org.opencontainers.image.description="Symbol-aware code indexer + agent runtime"
LABEL org.opencontainers.image.licenses="MIT"

# Working dir is the volume mount point — `docker run -v $PWD:/work`
# pattern. crabcc reads the cwd as the repo to index.
WORKDIR /work

COPY --from=build /src/target/release/crabcc /usr/local/bin/crabcc

USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/crabcc"]
