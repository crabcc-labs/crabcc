#!/usr/bin/env bash
# crabcc install script — curl https://raw.githubusercontent.com/peterlodri-sec/crabcc/main/install.sh | bash
#
# Detects host OS + arch, downloads the matching release binary from GitHub,
# verifies the sha256 checksum, and installs to ~/.local/bin (or, if that's
# not writable, to /usr/local/bin via sudo). No fancy package manager — just
# a 200-line bash script that does the right thing on a clean machine.

set -euo pipefail

# ---------- config ----------
REPO="peterlodri-sec/crabcc"
BIN_NAME="crabcc"
INSTALL_DIR="${CRABCC_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${CRABCC_VERSION:-latest}"   # `latest` or a specific tag like `v1.0.0`

# ---------- helpers ----------
say()  { printf '\033[1;32m▌\033[0m %s\n' "$*" >&2; }
warn() { printf '\033[1;33m▌\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m▌\033[0m %s\n' "$*" >&2; exit 1; }

require() {
    command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1 (please install it and re-run)"
}

# ---------- preflight ----------
require curl
require tar
require shasum

# ---------- detect OS + arch ----------
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
    linux)  PLATFORM_OS="unknown-linux-gnu" ;;
    darwin) PLATFORM_OS="apple-darwin" ;;
    *)      die "unsupported OS: $OS (crabcc supports linux + macos)" ;;
esac

case "$ARCH" in
    x86_64|amd64) PLATFORM_ARCH="x86_64" ;;
    aarch64|arm64) PLATFORM_ARCH="aarch64" ;;
    *) die "unsupported arch: $ARCH" ;;
esac

TARGET="${PLATFORM_ARCH}-${PLATFORM_OS}"
say "target: ${TARGET}"

# ---------- resolve version ----------
if [[ "$VERSION" == "latest" ]]; then
    say "resolving latest release..."
    VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
              | grep -E '"tag_name"' | head -1 | sed -E 's/.*"([^"]+)".*/\1/')"
    [[ -n "$VERSION" ]] || die "could not resolve latest version (check $REPO has a published release)"
fi
say "version: $VERSION"

# ---------- download ----------
ARCHIVE="${BIN_NAME}-${VERSION}-${TARGET}.tar.gz"
URL="https://github.com/$REPO/releases/download/$VERSION/$ARCHIVE"
SHA_URL="$URL.sha256"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

say "downloading $URL ..."
curl -fsSL --output "$TMP/$ARCHIVE" "$URL" \
    || die "download failed (does $VERSION ship a $TARGET artifact?)"

# ---------- verify sha256 (tolerate missing checksum gracefully) ----------
if curl -fsSL --output "$TMP/$ARCHIVE.sha256" "$SHA_URL" 2>/dev/null; then
    say "verifying sha256..."
    EXPECTED="$(awk '{print $1}' "$TMP/$ARCHIVE.sha256")"
    ACTUAL="$(shasum -a 256 "$TMP/$ARCHIVE" | awk '{print $1}')"
    if [[ "$EXPECTED" != "$ACTUAL" ]]; then
        die "sha256 mismatch:\n  expected: $EXPECTED\n  actual:   $ACTUAL"
    fi
    say "sha256 ok"
else
    warn "no .sha256 sibling found; skipping checksum verification"
fi

# ---------- unpack ----------
say "unpacking..."
tar -xzf "$TMP/$ARCHIVE" -C "$TMP"

# Find the binary; release.yml packs it into a versioned subdir.
BIN_PATH="$(find "$TMP" -type f -name "$BIN_NAME" -perm -u+x | head -1)"
[[ -n "$BIN_PATH" ]] || die "could not find $BIN_NAME in archive"

# ---------- install ----------
mkdir -p "$INSTALL_DIR" 2>/dev/null || true
if [[ -w "$INSTALL_DIR" ]] || mkdir -p "$INSTALL_DIR"; then
    install -m 0755 "$BIN_PATH" "$INSTALL_DIR/$BIN_NAME"
    say "installed: $INSTALL_DIR/$BIN_NAME"
else
    say "$INSTALL_DIR not writable; using sudo to install to /usr/local/bin"
    sudo install -m 0755 "$BIN_PATH" "/usr/local/bin/$BIN_NAME"
    say "installed: /usr/local/bin/$BIN_NAME"
fi

# ---------- post-install hint ----------
if ! command -v "$BIN_NAME" >/dev/null 2>&1; then
    warn "$BIN_NAME is not on PATH yet."
    warn "Add this to your shell rc (~/.bashrc / ~/.zshrc):"
    warn "    export PATH=\"$INSTALL_DIR:\$PATH\""
else
    "$BIN_NAME" --version
fi

cat <<'EOF'

▌ crabcc installed.

next steps:
    cd <your-repo>
    crabcc index             # one-time, ~5–30s on a 13k-file repo
    crabcc sym Foo           # find a symbol
    crabcc callers Foo       # find call sites
    crabcc files --ext rb    # list indexed Ruby files
    crabcc watch             # auto-refresh on file changes (Ctrl-C to stop)

docs: https://github.com/peterlodri-sec/crabcc
EOF
