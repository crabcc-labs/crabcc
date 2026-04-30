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
# - build-essential / pkg-config: stock native-build prerequisites
# - libssl-dev / libsqlite3-dev: optional system links for crates that flip
#   off the bundled feature; no harm having them present
# - git / ca-certificates / curl: cargo fetch (git deps) + HTTPS
# - jq: the smoke step pipes tool output through jq
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential pkg-config libssl-dev libsqlite3-dev \
      git ca-certificates curl jq \
 && rm -rf /var/lib/apt/lists/*

# sccache — ~60-80% recompile-time reduction with a shared cache volume.
# Mount `crabcc-sccache:/root/.cache/sccache` to persist across runs.
RUN cargo install sccache --locked
ENV RUSTC_WRAPPER=sccache
ENV CARGO_INCREMENTAL=0

# cargo-nextest — CI uses it for the JUnit reporter; preinstall so the first
# `cargo nextest run` inside the container is instant rather than a build wait.
RUN cargo install cargo-nextest --locked

WORKDIR /work

# Default to a sanity check so `docker run crabcc-build` (no args) is useful.
CMD ["cargo", "--version"]
