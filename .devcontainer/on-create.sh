#!/usr/bin/env bash
# Runs ONCE when the container is created. Heavy installs go here.
set -euo pipefail

echo "[on-create] installing taskfile + cargo extensions"

# Taskfile (https://taskfile.dev) — the canonical entry point for this repo.
sh -c "$(curl --location https://taskfile.dev/install.sh)" -- -d -b /usr/local/bin

# Cargo helpers used by `task ci` / `task lint` / `task release`.
cargo install --locked cargo-nextest cargo-deny cargo-audit cargo-edit just || true

# Rust components.
rustup component add clippy rustfmt rust-src

echo "[on-create] done"
