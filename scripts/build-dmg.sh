#!/usr/bin/env bash
# build-dmg.sh — produce dist/crabcc-<version>.dmg.
#
# Stages installer/Crabcc.app, populates Contents/Resources with bundled
# binaries + skills + commands + install-aliases.sh, compiles the menubar
# Swift shim, ad-hoc codesigns the .app, and packages with hdiutil.
#
# Idempotent: deletes dist/ and any leftover build/ before each run.
#
# Usage:
#   scripts/build-dmg.sh                   # release-mode build
#   scripts/build-dmg.sh --skip-build      # reuse target/release/{crabcc,ccc}

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
SRC_APP="$REPO_ROOT/installer/Crabcc.app"
BUILD_DIR="$REPO_ROOT/build/dmg"
DIST_DIR="$REPO_ROOT/dist"
APP_STAGE="$BUILD_DIR/Crabcc.app"

# Pull version from the workspace Cargo.toml.
VERSION="$(awk -F'"' '/^version[[:space:]]*=/ {print $2; exit}' "$REPO_ROOT/Cargo.toml")"
[[ -n "$VERSION" ]] || { echo "could not read crate version" >&2; exit 1; }

DMG_NAME="crabcc-$VERSION"
DMG_PATH="$DIST_DIR/$DMG_NAME.dmg"

SKIP_BUILD=0
[[ "${1:-}" == "--skip-build" ]] && SKIP_BUILD=1

log() { printf '[build-dmg] %s\n' "$*"; }

# --- 1. compile binaries --------------------------------------------------

if [[ $SKIP_BUILD -eq 0 ]]; then
    log "cargo build --release -p crabcc-cli"
    cargo build --release -p crabcc-cli --manifest-path "$REPO_ROOT/Cargo.toml"
fi

CRABCC_BIN="$REPO_ROOT/target/release/crabcc"
CCC_BIN="$REPO_ROOT/target/release/ccc"
[[ -x "$CRABCC_BIN" ]] || { echo "missing $CRABCC_BIN" >&2; exit 1; }
[[ -x "$CCC_BIN"    ]] || { echo "missing $CCC_BIN"    >&2; exit 1; }

# --- 2. stage app bundle --------------------------------------------------

rm -rf "$BUILD_DIR" "$DIST_DIR"
mkdir -p "$BUILD_DIR" "$DIST_DIR"
cp -R "$SRC_APP" "$APP_STAGE"

# inject version into Info.plist
sed -i '' "s/__VERSION__/$VERSION/g" "$APP_STAGE/Contents/Info.plist"

mkdir -p "$APP_STAGE/Contents/Resources/bin" \
         "$APP_STAGE/Contents/Resources/skills" \
         "$APP_STAGE/Contents/Resources/commands"

cp "$CRABCC_BIN" "$APP_STAGE/Contents/Resources/bin/crabcc"
cp "$CCC_BIN"    "$APP_STAGE/Contents/Resources/bin/ccc"
chmod 0755 "$APP_STAGE/Contents/Resources/bin/"*

# Verify the copy actually landed — without this, a silently empty bin/ ships
# in the DMG and install.sh can't find the binaries at runtime.
for b in crabcc ccc; do
    [[ -x "$APP_STAGE/Contents/Resources/bin/$b" ]] \
        || { echo "build-dmg: $b binary missing/non-exec at $APP_STAGE/Contents/Resources/bin/$b" >&2; exit 1; }
done
log "bundled binaries verified (crabcc + ccc are present and executable)"

# skills (symlink-friendly copy: dereference, since DMG can't ship symlinks safely)
for s in "$REPO_ROOT/skill"/*/; do
    [[ -d "$s" ]] || continue
    name="$(basename "$s")"
    mkdir -p "$APP_STAGE/Contents/Resources/skills/$name"
    cp -L "$s"*.md "$APP_STAGE/Contents/Resources/skills/$name/" 2>/dev/null || true
done

# commands
for c in "$REPO_ROOT/commands"/*; do
    [[ -e "$c" ]] || continue
    if [[ -d "$c" ]]; then
        name="$(basename "$c")"
        mkdir -p "$APP_STAGE/Contents/Resources/commands/$name"
        cp -L "$c"/*.md "$APP_STAGE/Contents/Resources/commands/$name/" 2>/dev/null || true
    else
        cp -L "$c" "$APP_STAGE/Contents/Resources/commands/"
    fi
done

# install-aliases.sh + version.sh (sourced by it)
cp "$REPO_ROOT/scripts/install-aliases.sh" "$APP_STAGE/Contents/Resources/install-aliases.sh"
cp "$REPO_ROOT/scripts/version.sh"         "$APP_STAGE/Contents/Resources/version.sh" 2>/dev/null || true
chmod 0755 "$APP_STAGE/Contents/Resources/install-aliases.sh"

# --- 3. compile menubar.swift ---------------------------------------------

if ! command -v swiftc >/dev/null 2>&1; then
    echo "swiftc not found — install Xcode Command Line Tools (xcode-select --install)" >&2
    exit 1
fi

log "swiftc *.swift -> Crabcc"
# `-parse-as-library` is required once the source set crosses one file
# (sticky.swift was added in #189 phase 0).
# Top-level expressions live inside `@main CrabccMenubarApp` in menubar.swift.
# Globbing the source set means future additions need no build-script churn.
shopt -s nullglob
SWIFT_SRCS=( "$APP_STAGE/Contents/MacOS"/*.swift )
shopt -u nullglob
if [ "${#SWIFT_SRCS[@]}" -eq 0 ]; then
    echo "no Swift sources found in $APP_STAGE/Contents/MacOS" >&2
    exit 1
fi
swiftc -O -parse-as-library -target arm64-apple-macos13.0 \
    -o "$APP_STAGE/Contents/MacOS/Crabcc" \
    "${SWIFT_SRCS[@]}"
rm "${SWIFT_SRCS[@]}"

chmod 0755 "$APP_STAGE/Contents/MacOS/Crabcc"
chmod 0644 "$APP_STAGE/Contents/Resources/scripts/"*.sh

# --- 4. ad-hoc codesign ---------------------------------------------------

# Sign nested Mach-O binaries (Resources/bin/) first — they're not bundle
# main-exec, so codesign treats them as standalone files.
for b in crabcc ccc; do
    /usr/bin/codesign --force --sign - "$APP_STAGE/Contents/Resources/bin/$b"
done
# Sign the bundle as a whole. This hashes the main exec (Contents/MacOS/Crabcc)
# and writes the bundle's CodeResources manifest (which covers shell scripts
# in Helpers/ as resources, not code). NO --deep — that's deprecated and the
# explicit sign above already covered nested Mach-O.
/usr/bin/codesign --force --sign - "$APP_STAGE"
log "ad-hoc signed $APP_STAGE"

# Verify
/usr/bin/codesign --verify --deep --strict "$APP_STAGE" \
    && log "codesign verify: ok" \
    || { echo "codesign verify failed" >&2; exit 1; }

# --- 5. package into DMG --------------------------------------------------

# Drag-to-install layout: Crabcc.app + a symlink to /Applications.
DMG_STAGE="$BUILD_DIR/dmg-stage"
mkdir -p "$DMG_STAGE"
cp -R "$APP_STAGE" "$DMG_STAGE/Crabcc.app"
ln -s /Applications "$DMG_STAGE/Applications"

log "hdiutil create $DMG_PATH"
hdiutil create -volname "Crabcc $VERSION" \
               -srcfolder "$DMG_STAGE" \
               -ov -format UDZO \
               "$DMG_PATH" >/dev/null

# Codesign the DMG itself (ad-hoc) so it's recognized as a signed image.
/usr/bin/codesign --force --sign - "$DMG_PATH" 2>/dev/null || true

log "done: $DMG_PATH ($(du -h "$DMG_PATH" | awk '{print $1}'))"
