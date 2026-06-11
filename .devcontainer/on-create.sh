#!/usr/bin/env bash
# Runs ONCE when the container is created. Heavy installs go here.
set -euo pipefail

echo "[on-create] installing system deps + taskfile + cargo extensions"

# System build deps: mold linker (3-5x faster than ld on this workspace),
# SQLite dev headers (rusqlite), OpenSSL headers (ureq), clang (mold front-end).
sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends \
    clang mold libsqlite3-dev libssl-dev pkg-config

# Taskfile (https://taskfile.dev) — the canonical entry point for this repo.
sh -c "$(curl --location https://taskfile.dev/install.sh)" -- -d -b /usr/local/bin

# sccache: per-file rustc cache; dramatically speeds up incremental rebuilds.
cargo install --locked sccache \
    || echo "[on-create] WARN: sccache install failed — builds will still work"

# Cargo helpers used by `task ci` / `task lint` / `task release`.
# Install one-by-one so a single failure is visible rather than swallowed.
for _tool in cargo-nextest cargo-deny cargo-audit cargo-edit just; do
    cargo install --locked "$_tool" \
        || echo "[on-create] WARN: failed to install $_tool, continuing"
done

# Rust components.
rustup component add clippy rustfmt rust-src

# Claude Code CLI — requires Node (installed via devcontainer feature above).
npm install -g @anthropic-ai/claude-code \
    || echo "[on-create] WARN: claude-code CLI install failed — 'claude' will not be available"

echo "[on-create] done"
