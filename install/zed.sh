#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: install/zed.sh
#
# One-shot installer for the crabcc Zed integration. Run it from any bash
# shell — no UI clicking required for the parts that can be automated:
#
#   bash install/zed.sh            # from a crabcc checkout
#   curl -fsSL <raw-url>/install/zed.sh | bash   # standalone (clones first)
#
# What it does (idempotent — re-run any time):
#
#   1. Locate (or clone) a crabcc checkout.
#   2. `cargo install` the `ucracc-lsp` binary onto $PATH
#      (honours --features; defaults to the nav-only build).
#   3. Build the `.crabcc` index for a project (default: $PWD if it looks
#      like one; override with --index, skip with --no-index).
#   4. Ensure the `wasm32-wasip1` Rust target and build-check the Zed
#      extension so toolchain problems surface here, not in Zed.
#   5. Register the extension with Zed:
#        - default: stage it + print the single `zed: install dev
#          extension` palette action (Zed compiles it with its own
#          bundled, version-matched toolchain — the reliable path).
#        - --headless (experimental): compile the wasm component with
#          `wasm-tools` + a wasi reactor adapter and drop it straight into
#          Zed's installed-extensions dir, which Zed rescans on next
#          launch. No UI, but you must restart Zed.
#
# Why not fully headless by default? Zed has no stable CLI to install an
# unpublished/dev extension (zed-industries/zed#10943), and it compiles
# extensions with an internally-bundled component toolchain matched to the
# running Zed's API version. The --headless path reproduces that compile
# externally; it works, but it's version-sensitive, so it's opt-in.
#
# Flags:
#   --features <list>    cargo features for ucracc-lsp (e.g. memory,fetch,rerank)
#   --index <dir>        build the .crabcc index in <dir>
#   --no-index           skip the index build
#   --headless           drop a compiled component into Zed's installed/ dir
#   --zed-ext-dir <dir>  override Zed's extensions dir (auto-detected by OS)
#   --bin-dir <dir>      cargo install dir (default: ~/.cargo/bin)
#   --force              cargo install --force (rebuild even if up to date)
#   --help, -h           print this header
#
# Environment:
#   CRABCC_DIR           path to a crabcc checkout (else auto-detected/cloned)
#   CRABCC_REPO          git remote for the standalone clone path
#   ZED_WASI_ADAPTER     path to wasi_snapshot_preview1.reactor.wasm
#                        (--headless; else fetched from the wasmtime release)
#
# Exit codes:  0 success · 1 missing tool / build / clone failure
# ---------------------------------------------------------------------------
set -euo pipefail

# --- terminal styling ------------------------------------------------------
if [ -t 1 ] && command -v tput >/dev/null 2>&1; then
    BOLD="$(tput bold || true)"; DIM="$(tput dim || true)"
    RED="$(tput setaf 1 || true)"; GREEN="$(tput setaf 2 || true)"
    YELLOW="$(tput setaf 3 || true)"; BLUE="$(tput setaf 4 || true)"
    RESET="$(tput sgr0 || true)"
else
    BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; BLUE=""; RESET=""
fi
say()  { printf '%s\n' "${BLUE}${BOLD}::${RESET} $*"; }
ok()   { printf '%s\n' "${GREEN}✓${RESET} $*"; }
warn() { printf '%s\n' "${YELLOW}!${RESET} $*" >&2; }
die()  { printf '%s\n' "${RED}✗${RESET} $*" >&2; exit 1; }

# --- defaults --------------------------------------------------------------
FEATURES=""
INDEX_DIR=""
DO_INDEX="auto"
HEADLESS=0
ZED_EXT_DIR="${ZED_EXT_DIR:-}"
BIN_DIR="${CRABCC_INSTALL_DIR:-$HOME/.cargo/bin}"
FORCE=0
CRABCC_REPO="${CRABCC_REPO:-https://github.com/crabcc-labs/crabcc.git}"
EXT_ID="crabcc"
SERVER_BIN="ucracc-lsp"

# --- args ------------------------------------------------------------------
while [ $# -gt 0 ]; do
    case "$1" in
        --features) FEATURES="${2:-}"; shift 2 ;;
        --features=*) FEATURES="${1#*=}"; shift ;;
        --index) INDEX_DIR="${2:-}"; DO_INDEX="yes"; shift 2 ;;
        --index=*) INDEX_DIR="${1#*=}"; DO_INDEX="yes"; shift ;;
        --no-index) DO_INDEX="no"; shift ;;
        --headless) HEADLESS=1; shift ;;
        --zed-ext-dir) ZED_EXT_DIR="${2:-}"; shift 2 ;;
        --zed-ext-dir=*) ZED_EXT_DIR="${1#*=}"; shift ;;
        --bin-dir) BIN_DIR="${2:-}"; shift 2 ;;
        --bin-dir=*) BIN_DIR="${1#*=}"; shift ;;
        --force) FORCE=1; shift ;;
        -h|--help)
            # Print the banner comment block (lines 2..first non-comment).
            sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//; s/^#//' | sed '$d'
            exit 0 ;;
        *) die "unknown flag: $1 (try --help)" ;;
    esac
done

command -v cargo >/dev/null 2>&1 || die "cargo not found — install Rust from https://rustup.rs and re-run."

# --- 1. locate / clone the repo -------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
CLONED_TMP=""
find_repo() {
    # Explicit override wins.
    if [ -n "${CRABCC_DIR:-}" ] && [ -f "$CRABCC_DIR/crates/ucracc-lsp/Cargo.toml" ]; then
        printf '%s' "$CRABCC_DIR"; return 0
    fi
    # Script running from inside a checkout (install/zed.sh → repo root).
    if [ -f "$SCRIPT_DIR/../crates/ucracc-lsp/Cargo.toml" ]; then
        ( cd "$SCRIPT_DIR/.." && pwd ); return 0
    fi
    # Current dir.
    if [ -f "./crates/ucracc-lsp/Cargo.toml" ]; then pwd; return 0; fi
    return 1
}
if REPO="$(find_repo)"; then
    ok "crabcc checkout: ${BOLD}$REPO${RESET}"
else
    command -v git >/dev/null 2>&1 || die "no crabcc checkout found and git is unavailable to clone one. Set CRABCC_DIR."
    CLONED_TMP="$(mktemp -d)"
    say "cloning crabcc into $CLONED_TMP …"
    git clone --depth 1 "$CRABCC_REPO" "$CLONED_TMP" >/dev/null 2>&1 \
        || die "git clone failed ($CRABCC_REPO)."
    REPO="$CLONED_TMP"
    ok "cloned crabcc."
fi
cleanup() { [ -n "$CLONED_TMP" ] && rm -rf "$CLONED_TMP" || true; }
trap cleanup EXIT
EXT_SRC="$REPO/editors/zed/crabcc"
[ -d "$EXT_SRC" ] || die "Zed extension source not found at $EXT_SRC (old checkout?)."

# --- 2. install the ucracc-lsp binary -------------------------------------
# `cargo install --root R` always writes the executable to `R/bin/`. We
# treat --bin-dir as the *final* directory the binary should land in, so we
# derive the root by stripping a trailing `/bin` and report the real
# `R/bin` location — that way the PATH advice never points at the wrong dir
# (e.g. --bin-dir /opt/tools would otherwise install to /opt/tools/bin but
# tell the user to add /opt/tools).
INSTALL_ARGS=(install --path "$REPO/crates/ucracc-lsp" --locked)
EFFECTIVE_BIN_DIR="$BIN_DIR"
if [ "$BIN_DIR" != "$HOME/.cargo/bin" ]; then
    INSTALL_ROOT="${BIN_DIR%/bin}"
    INSTALL_ARGS+=(--root "$INSTALL_ROOT")
    EFFECTIVE_BIN_DIR="$INSTALL_ROOT/bin"
fi
[ "$FORCE" = 1 ] && INSTALL_ARGS+=(--force)
[ -n "$FEATURES" ] && INSTALL_ARGS+=(--features "$FEATURES")
say "installing ${SERVER_BIN} → $EFFECTIVE_BIN_DIR"
if ! cargo "${INSTALL_ARGS[@]}"; then
    die "cargo install of ${SERVER_BIN} failed."
fi
if command -v "$SERVER_BIN" >/dev/null 2>&1; then
    ok "${SERVER_BIN} on \$PATH: $(command -v "$SERVER_BIN")"
else
    warn "${SERVER_BIN} installed to $EFFECTIVE_BIN_DIR but it's not on \$PATH."
    warn "add it:  ${BOLD}export PATH=\"$EFFECTIVE_BIN_DIR:\$PATH\"${RESET}  (in your shell rc)"
fi

# --- 3. build the index ----------------------------------------------------
if [ "$DO_INDEX" = "auto" ]; then
    if [ -d ".git" ] || [ -f "Cargo.toml" ] || [ -f "package.json" ] || [ -f "go.mod" ]; then
        INDEX_DIR="$PWD"; DO_INDEX="yes"
    else
        DO_INDEX="no"
    fi
fi
if [ "$DO_INDEX" = "yes" ]; then
    if command -v crabcc >/dev/null 2>&1; then
        say "building index in ${BOLD}$INDEX_DIR${RESET}"
        ( cd "$INDEX_DIR" && crabcc index ) && ok "index built (.crabcc/)" \
            || warn "crabcc index failed — run it manually in your project."
    else
        warn "crabcc CLI not found; skipping index. Install it (cargo install --path crates/crabcc-cli) and run 'crabcc index' in your project."
    fi
else
    say "skipping index build (use --index <dir> to build one)."
fi

# --- 4. toolchain + extension build-check ---------------------------------
say "checking the wasm32-wasip1 toolchain"
if command -v rustup >/dev/null 2>&1; then
    rustup target add wasm32-wasip1 >/dev/null 2>&1 || warn "could not add wasm32-wasip1 target via rustup."
else
    warn "rustup not found — ensure the wasm32-wasip1 target is installed for your toolchain."
fi
CORE_WASM=""
say "build-checking the Zed extension"
if ( cd "$EXT_SRC" && cargo build --release --target wasm32-wasip1 ) >/dev/null 2>&1; then
    CORE_WASM="$EXT_SRC/target/wasm32-wasip1/release/zed_crabcc.wasm"
    [ -f "$CORE_WASM" ] || CORE_WASM="$(find "$EXT_SRC/target/wasm32-wasip1/release" -maxdepth 1 -name '*.wasm' 2>/dev/null | head -1)"
    ok "extension compiles."
else
    warn "extension build-check failed (you can still install it from Zed, which compiles it itself)."
fi

# --- 5. register with Zed --------------------------------------------------
# A dev extension is loaded from its *source directory* on every Zed launch,
# so that directory has to persist. When we cloned into a tempdir (standalone
# `curl | bash` mode), the EXIT trap would wipe it out from under the user —
# so stage a durable copy of editors/zed/crabcc and point the instructions there.
# Running from a real checkout keeps pointing at the live tree (so a
# `git pull` + "rebuild dev extension" picks up changes).
STAGE_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/crabcc/zed-extension"
EXT_INSTALL_DIR="$EXT_SRC"
if [ -n "$CLONED_TMP" ]; then
    mkdir -p "$STAGE_DIR"
    if command -v rsync >/dev/null 2>&1; then
        rsync -a --delete --exclude target "$EXT_SRC"/ "$STAGE_DIR"/
    else
        rm -rf "$STAGE_DIR"; mkdir -p "$STAGE_DIR"
        ( cd "$EXT_SRC" && tar --exclude=target -cf - . ) | ( cd "$STAGE_DIR" && tar -xf - )
    fi
    EXT_INSTALL_DIR="$STAGE_DIR"
    ok "staged the extension to a durable path: ${BOLD}$STAGE_DIR${RESET}"
fi

detect_zed_ext_dir() {
    [ -n "$ZED_EXT_DIR" ] && { printf '%s' "$ZED_EXT_DIR"; return; }
    case "$(uname -s)" in
        Darwin) printf '%s' "$HOME/Library/Application Support/Zed/extensions" ;;
        *)      printf '%s' "${XDG_DATA_HOME:-$HOME/.local/share}/zed/extensions" ;;
    esac
}
ZDIR="$(detect_zed_ext_dir)"

manual_instructions() {
    cat <<EOF

${BOLD}Final step — register the extension in Zed (one action):${RESET}
  1. Open Zed.
  2. Command palette (${BOLD}cmd-shift-p${RESET}) → ${BOLD}zed: install dev extension${RESET}
  3. Select this directory:
       ${BOLD}$EXT_INSTALL_DIR${RESET}

Zed compiles it with its own version-matched toolchain and binds
${SERVER_BIN} to Rust / TS / JS / Python / Ruby / Go / Swift / Java / YAML /
Markdown. Open a file in an indexed project and try ${BOLD}go to definition${RESET}
or ${BOLD}project: open symbol${RESET}.
EOF
}

if [ "$HEADLESS" = 1 ]; then
    say "headless extension install (experimental)"
    if [ -z "$CORE_WASM" ] || [ ! -f "$CORE_WASM" ]; then
        warn "no compiled wasm to package — falling back to the manual step."
        manual_instructions
    elif ! command -v wasm-tools >/dev/null 2>&1; then
        warn "wasm-tools not found (cargo install wasm-tools). Falling back to the manual step."
        manual_instructions
    else
        ADAPTER="${ZED_WASI_ADAPTER:-}"
        if [ -z "$ADAPTER" ]; then
            ADAPTER="$(mktemp -d)/wasi_snapshot_preview1.reactor.wasm"
            ADAPTER_URL="https://github.com/bytecodealliance/wasmtime/releases/download/v21.0.1/wasi_snapshot_preview1.reactor.wasm"
            if command -v curl >/dev/null 2>&1 && curl -fsSL "$ADAPTER_URL" -o "$ADAPTER" 2>/dev/null; then
                :
            else
                warn "could not fetch the wasi reactor adapter (set ZED_WASI_ADAPTER to a local copy). Falling back to the manual step."
                ADAPTER=""
            fi
        fi
        if [ -n "$ADAPTER" ] && [ -f "$ADAPTER" ]; then
            DEST="$ZDIR/installed/$EXT_ID"
            mkdir -p "$DEST"
            if wasm-tools component new "$CORE_WASM" --adapt "wasi_snapshot_preview1=$ADAPTER" -o "$DEST/extension.wasm" 2>/dev/null; then
                cp "$EXT_SRC/extension.toml" "$DEST/extension.toml"
                ok "dropped a compiled component into ${BOLD}$DEST${RESET}"
                warn "restart Zed to pick it up. If it fails to load, your Zed's"
                warn "extension API version may differ — use the reliable path instead:"
                manual_instructions
            else
                warn "component encoding failed — falling back to the manual step."
                manual_instructions
            fi
        else
            manual_instructions
        fi
    fi
else
    if [ -d "$ZDIR" ]; then
        ok "detected Zed extensions dir: ${DIM}$ZDIR${RESET}"
    else
        warn "Zed extensions dir not found at $ZDIR (is Zed installed?)."
    fi
    manual_instructions
fi

echo
ok "${BOLD}Done.${RESET} Deep guide: ${BOLD}$REPO/crates/ucracc-lsp/docs/ZED.md${RESET}"
