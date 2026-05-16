# Fast, reusable build base for crabcc — local hermetic builds + CI container.
#
# Build:  docker build -t ghcr.io/peterlodri-sec/crabcc-build:latest .
# Local:  docker run --rm -v "$PWD":/work -w /work \
#             -v crabcc-cargo:/usr/local/cargo/registry \
#             -v crabcc-sccache:/root/.cache/sccache \
#             ghcr.io/peterlodri-sec/crabcc-build:latest cargo test --workspace
# CI:     jobs.<id>.container.image: ghcr.io/peterlodri-sec/crabcc-build:latest
#
# The image installs only the build tools — sources are mounted at runtime.
# Persist `cargo registry` and `sccache` via named volumes for fast incremental
# rebuilds across runs.

FROM rust:1.86-slim

# System deps:
# - build-essential / pkg-config: cc + native-build prerequisites
# - libssl-dev / libsqlite3-dev: optional system links
# - git / ca-certificates / curl: cargo fetch (git deps) + HTTPS
# - jq: the smoke step pipes tool output through jq
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential pkg-config libssl-dev libsqlite3-dev \
      git ca-certificates curl jq \
 && rm -rf /var/lib/apt/lists/*

# CC=cc: consistent C compiler for build scripts.
# wild linker: pure-Rust, ~3-5x faster link than GNU ld.
ENV CC=cc
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=cc
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=wild"
ENV CARGO_INCREMENTAL=0

# sccache — ~60-80% recompile-time reduction with a shared cache volume.
# Mount `crabcc-sccache:/root/.cache/sccache` to persist across runs.
# wild, sccache, and nextest are installed together so they share one
# registry fetch and land in the same image layer.
RUN cargo install sccache wild cargo-nextest --locked
ENV RUSTC_WRAPPER=sccache

WORKDIR /work

# Default to a sanity check so `docker run crabcc-build` (no args) is useful.
CMD ["cargo", "--version"]
