#!/usr/bin/env bash
# crabcc minimal kernel — build script for Apple Containers
#
# Produces a stripped vmlinuz (Image.gz) for Apple's Virtualization.framework.
# Reference: https://github.com/apple/containerization/tree/main/kernel
#
# Usage:
#   install/kernel/build.sh               # uses LINUX_SRC or clones 6.6
#   LINUX_SRC=/path/to/linux install/kernel/build.sh
#
# Output: install/kernel/vmlinuz (copy into your container image)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
LINUX_VERSION="${LINUX_VERSION:-6.6.90}"
LINUX_SRC="${LINUX_SRC:-/tmp/linux-${LINUX_VERSION}}"
OUT="$SCRIPT_DIR/vmlinuz"
JOBS="${JOBS:-$(sysctl -n hw.physicalcpu 2>/dev/null || nproc)}"

# ── 1. Ensure cross-compile toolchain (aarch64-linux-gnu) ────────────────────
if ! command -v aarch64-linux-gnu-gcc &>/dev/null; then
    if command -v brew &>/dev/null; then
        brew install aarch64-elf-gcc
        export CROSS_COMPILE=aarch64-elf-
    else
        echo "Install aarch64 cross-compiler: brew install aarch64-elf-gcc" >&2
        exit 1
    fi
else
    export CROSS_COMPILE=aarch64-linux-gnu-
fi
export ARCH=arm64

# ── 2. Fetch Linux source ─────────────────────────────────────────────────────
if [ ! -d "$LINUX_SRC" ]; then
    echo "▶ Fetching Linux ${LINUX_VERSION}..."
    MAJOR="${LINUX_VERSION%%.*}"
    curl -fsSL \
        "https://cdn.kernel.org/pub/linux/kernel/v${MAJOR}.x/linux-${LINUX_VERSION}.tar.xz" \
        | tar -xJ -C "$(dirname "$LINUX_SRC")"
fi

cd "$LINUX_SRC"

# ── 3. Merge config ────────────────────────────────────────────────────────────
echo "▶ Configuring kernel..."
make defconfig
scripts/kconfig/merge_config.sh -m .config "$SCRIPT_DIR/config.fragment"
make olddefconfig

# ── 4. Build ──────────────────────────────────────────────────────────────────
echo "▶ Building kernel (j=$JOBS)..."
make -j"$JOBS" Image.gz

# ── 5. Copy output ────────────────────────────────────────────────────────────
cp arch/arm64/boot/Image.gz "$OUT"
echo "▶ Built: $OUT ($(du -h "$OUT" | cut -f1))"
echo ""
echo "Use with Apple Containers:"
echo "  container run --kernel $OUT ..."
echo "Or add to docker-compose as a custom kernel mount for supported runtimes."
