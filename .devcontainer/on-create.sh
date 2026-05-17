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

# happy (https://happy.engineering, https://github.com/slopus/happy) — keeps
# a session alive across codespace sleeps so a CLI agent can resume without
# re-handshaking. The daemon is started by post-create.sh / post-start.sh;
# here we only install the binary.
echo "[on-create] installing happy"
if command -v npm >/dev/null 2>&1; then
  npm install -g happy || echo "[on-create] WARN: happy install failed (non-fatal)" >&2
else
  echo "[on-create] WARN: npm not available — skipping happy install" >&2
fi

echo "[on-create] done"
