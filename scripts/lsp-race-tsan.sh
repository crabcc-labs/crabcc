#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/lsp-race-tsan.sh
#
# Run the ucracc-lsp concurrency suite (crates/ucracc-lsp/tests/concurrency.rs)
# under ThreadSanitizer. The plain `cargo test --test concurrency` run only
# catches races that *manifest* (a panic, a hang, a torn result); TSan catches
# the underlying *data races* (UB) directly, including benign-looking ones.
#
# TSan needs a nightly toolchain (the `-Zsanitizer` flag) and a freshly
# instrumented std (`-Zbuild-std`), so this is opt-in and intentionally not
# wired into CI.
#
# Usage:
#   scripts/lsp-race-tsan.sh                              # x86_64-unknown-linux-gnu
#   TARGET=aarch64-unknown-linux-gnu scripts/lsp-race-tsan.sh
#
# Prereqs:
#   rustup toolchain install nightly
#   rustup component add rust-src --toolchain nightly
# ---------------------------------------------------------------------------
set -euo pipefail

TARGET="${TARGET:-x86_64-unknown-linux-gnu}"

if ! rustup toolchain list 2>/dev/null | grep -q '^nightly'; then
  echo "error: nightly toolchain not installed. Run:" >&2
  echo "  rustup toolchain install nightly" >&2
  echo "  rustup component add rust-src --toolchain nightly" >&2
  exit 1
fi

# TSan can't operate under panic=abort; the `test` profile unwinds, so we're
# fine. Run the test *harness* single-threaded so sanitizer reports don't
# interleave — the parallelism we want to inspect lives in the tokio worker
# threads inside each test, not across the harness.
export RUSTFLAGS="-Zsanitizer=thread ${RUSTFLAGS:-}"
export RUSTDOCFLAGS="-Zsanitizer=thread ${RUSTDOCFLAGS:-}"
export TSAN_OPTIONS="${TSAN_OPTIONS:-halt_on_error=1:second_deadlock_stack=1}"

echo ">> ThreadSanitizer :: ucracc-lsp concurrency suite (target=$TARGET)"
exec cargo +nightly test \
  -Z build-std \
  --target "$TARGET" \
  -p ucracc-lsp --test concurrency \
  -- --test-threads=1 --nocapture
