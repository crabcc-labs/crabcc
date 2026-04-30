#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: install.sh
#
# Modern one-liner installer. Designed to be invoked verbatim:
#
#   gh api -H 'Accept: application/vnd.github.v3.raw' \
#       /repos/peterlodri-sec/crabcc/contents/install.sh | bash
#
# What it does in a single pass (idempotent — re-run any time):
#
#   1. Verify required tools (gh, cargo, git). Offer install hints if
#      missing.
#   2. Pick install dir (~/.cargo/bin by default, honours
#      $CRABCC_INSTALL_DIR).
#   3. Clone the source via gh into a tempdir, run `cargo install --path
#      crates/crabcc-cli --locked`, clean up.
#   4. Install shell completions for the user's current shell (zsh /
#      bash / fish) — detected via $SHELL, written into the right
#      rc-adjacent directory.
#   5. (Optional, when claude is present) link the skill + slash
#      commands under ~/.claude/ and suggest the MCP registration command.
#   6. Print a green `crabcc go` hint so the user lands on the new
#      one-shot bootstrap immediately.
#
# Upgrade semantics (issue #24):
#   - On every invocation, the script first probes for an existing
#     `crabcc` binary (at $INSTALL_DIR/$BIN_NAME or anywhere on PATH).
#   - If found, it compares the installed version against the latest
#     release tag on $CRABCC_REPO (`gh release list -L 1`), falling
#     back to `[workspace.package].version` from the default branch's
#     Cargo.toml when no releases are published.
#   - If both versions match → no-op build skip. Completions + Claude
#     symlinks are still refreshed (cheap; idempotent).
#   - If the installed version is older → cargo install --force.
#   - Pass `--force` to skip the short-circuit and rebuild regardless.
#
# Flags:
#   --no-completions     skip step 4 (shell completions)
#   --no-claude          skip step 5 (~/.claude/ symlinks)
#   --version=v2.3.0     install a specific tag (default: main HEAD)
#   --bin-dir=/path      override $CRABCC_INSTALL_DIR
#   --force              rebuild + reinstall even if local == latest
#   --check              report install-vs-latest delta and exit (no writes)
#   --help, -h           print this header
#
# Environment:
#   CRABCC_INSTALL_DIR   target dir (defaults to ~/.cargo/bin)
#   CRABCC_REPO          source repo (defaults to peterlodri-sec/crabcc;
#                        also honoured by `crabcc upgrade`)
#
# Exit codes:
#   0  success
#   1  missing required tool / build failed / clone failed
#
# ---------------------------------------------------------------------------
# CHANGELOG
#   v2.1.0 (2026-04-30) — upgrade-on-rerun semantics (closes #24). Detects
#                          an existing `crabcc` install, compares versions
#                          against the latest GitHub release tag, and
#                          short-circuits the build when nothing changed.
#                          Adds --force, --check.
#   v2.0.0 (2026-04-30) — rewrite. Cargo-first install (was prebuilt-only),
#                          auto-detect shell + claude, wire completions
#                          and slash-command symlinks in one pass. Closes
#                          the "three-command bootstrap" complaint in the
#                          README.
#   v1.0.0 (2026-04-29) — original prebuilt-binary downloader.
# ---------------------------------------------------------------------------

set -euo pipefail

# --- terminal styling -----------------------------------------------------
if [ -t 1 ] && command -v tput >/dev/null 2>&1; then
    BOLD="$(tput bold || true)"; DIM="$(tput dim || true)"
    GREEN="$(tput setaf 2 || true)"; YELLOW="$(tput setaf 3 || true)"
    RED="$(tput setaf 1 || true)"; BLUE="$(tput setaf 4 || true)"
    RESET="$(tput sgr0 || true)"
else
    BOLD=""; DIM=""; GREEN=""; YELLOW=""; RED=""; BLUE=""; RESET=""
fi
say()  { printf "${GREEN}▌${RESET} %s\n" "$*" >&2; }
warn() { printf "${YELLOW}▌${RESET} %s\n" "$*" >&2; }
die()  { printf "${RED}▌ error:${RESET} %s\n" "$*" >&2; exit 1; }

# --- argv -----------------------------------------------------------------
INSTALL_COMPLETIONS=1
INSTALL_CLAUDE=1
VERSION_ARG=""
BIN_DIR_OVERRIDE=""
FORCE=0
CHECK_ONLY=0
for arg in "$@"; do
    case "$arg" in
        --no-completions) INSTALL_COMPLETIONS=0 ;;
        --no-claude)      INSTALL_CLAUDE=0 ;;
        --version=*)      VERSION_ARG="${arg#*=}" ;;
        --bin-dir=*)      BIN_DIR_OVERRIDE="${arg#*=}" ;;
        --force)          FORCE=1 ;;
        --check)          CHECK_ONLY=1 ;;
        --help|-h)
            sed -n '2,60p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) die "unknown arg: $arg (try --help)" ;;
    esac
done

# --- config ---------------------------------------------------------------
REPO="${CRABCC_REPO:-peterlodri-sec/crabcc}"
BIN_NAME="crabcc"
INSTALL_DIR="${BIN_DIR_OVERRIDE:-${CRABCC_INSTALL_DIR:-$HOME/.cargo/bin}}"

# --- preflight ------------------------------------------------------------
say "${BOLD}crabcc installer${RESET} (repo: $REPO, target: $INSTALL_DIR)"

require_tool() {
    local tool="$1" hint="$2"
    if command -v "$tool" >/dev/null 2>&1; then
        say "✓ $tool: $(command -v "$tool")"
    else
        warn "✗ $tool not on PATH"
        warn "    install: $hint"
        return 1
    fi
}

missing=0
require_tool gh    "macOS \`brew install gh\`  •  Linux \`sudo apt-get install gh\` / \`sudo dnf install gh\`" || missing=$((missing + 1))
require_tool cargo "https://www.rust-lang.org/tools/install" || missing=$((missing + 1))
require_tool git   "macOS \`brew install git\`  •  Linux \`sudo apt-get install git\`" || missing=$((missing + 1))
[ "$missing" -eq 0 ] || die "$missing required tool(s) missing — install them and re-run."

if ! gh auth status >/dev/null 2>&1; then
    warn "gh is installed but not authenticated. Running \`gh auth login\` for you."
    gh auth login || die "gh auth login failed"
fi

# --- upgrade detection ----------------------------------------------------
# Probe for an existing install. If present and matching the latest
# remote version, short-circuit the build step so re-running install.sh
# becomes a fast no-op (issue #24). Refreshes completions + claude
# symlinks regardless — those are idempotent and cheap.
existing_path=""
if [ -x "$INSTALL_DIR/$BIN_NAME" ]; then
    existing_path="$INSTALL_DIR/$BIN_NAME"
elif command -v "$BIN_NAME" >/dev/null 2>&1; then
    existing_path="$(command -v "$BIN_NAME")"
fi

local_ver=""
if [ -n "$existing_path" ]; then
    # `crabcc --version` prints `crabcc <semver>`; isolate the semver field.
    local_ver="$("$existing_path" --version 2>/dev/null | awk '{print $2}' | head -1)"
    [ -n "$local_ver" ] && say "detected existing install: $existing_path (v$local_ver)"
fi

remote_ver=""
# Strategy 1 — pinned tag: VERSION_ARG was passed (`--version=v2.3.0`),
# treat it as the canonical target without remote lookup.
if [ -n "$VERSION_ARG" ]; then
    remote_ver="${VERSION_ARG#v}"
elif [ -n "$existing_path" ] || [ "$CHECK_ONLY" = "1" ]; then
    # Only spend the gh round-trip when we have an installed binary to
    # compare against (or the user explicitly asked via --check). Cold
    # installs don't need the version probe — they always build.
    say "checking $REPO for the latest release …"
    # Strategy 2 — latest release tag.
    remote_ver="$(
        gh release list --repo "$REPO" --limit 1 --json tagName --jq '.[0].tagName // ""' 2>/dev/null \
        | sed 's/^v//'
    )"
    # Strategy 3 — fall back to Cargo.toml on the default branch when no
    # releases have been cut yet. We grep the [workspace.package] block
    # for the version line; jq isn't required.
    if [ -z "$remote_ver" ]; then
        remote_ver="$(
            gh api -H "Accept: application/vnd.github.v3.raw" \
                "/repos/$REPO/contents/Cargo.toml" 2>/dev/null \
            | awk '
                /^\[workspace\.package\]/ { in_section = 1; next }
                /^\[/ { in_section = 0 }
                in_section && /^[[:space:]]*version[[:space:]]*=/ {
                    match($0, /"[^"]+"/)
                    if (RLENGTH > 0) {
                        print substr($0, RSTART + 1, RLENGTH - 2)
                        exit
                    }
                }
            '
        )"
    fi
fi
[ -n "$remote_ver" ] && say "remote version: v$remote_ver"

# --check just reports the delta and exits.
if [ "$CHECK_ONLY" = "1" ]; then
    if [ -z "$existing_path" ]; then
        say "no local install — would do a fresh \`cargo install\`."
    elif [ -z "$remote_ver" ]; then
        warn "could not resolve remote version — would attempt a rebuild."
    elif [ "$local_ver" = "$remote_ver" ]; then
        say "${BOLD}${GREEN}up to date${RESET}: local v$local_ver == remote v$remote_ver"
    else
        say "${BOLD}${YELLOW}update available${RESET}: local v$local_ver → remote v$remote_ver"
    fi
    exit 0
fi

# Decide whether to skip the (expensive) build step.
SKIP_BUILD=0
if [ "$FORCE" != "1" ] && [ -n "$local_ver" ] && [ -n "$remote_ver" ] \
        && [ "$local_ver" = "$remote_ver" ]; then
    say "${BOLD}${GREEN}already at v$local_ver${RESET} (no-op build skip; pass --force to rebuild)"
    SKIP_BUILD=1
fi

# --- step 1: clone + cargo install ----------------------------------------
TMP="$(mktemp -d -t crabcc-install.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT
say "cloning $REPO …"
gh repo clone "$REPO" "$TMP/src" -- --quiet --depth 1 ${VERSION_ARG:+--branch "$VERSION_ARG"}

if [ "$SKIP_BUILD" = "1" ]; then
    say "skipping build — local install is current."
else
    say "building (cargo install — this is the slow step, ~2–5 min on a cold cache)"
    mkdir -p "$INSTALL_DIR"
    (
        cd "$TMP/src"
        if [ "$INSTALL_DIR" = "$HOME/.cargo/bin" ]; then
            cargo install --path crates/crabcc-cli --locked --force
        else
            cargo build --release -p crabcc-cli --locked
            install -m 0755 "target/release/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
        fi
    )

    if ! command -v "$BIN_NAME" >/dev/null 2>&1 && [ ! -x "$INSTALL_DIR/$BIN_NAME" ]; then
        die "build appeared to succeed but $BIN_NAME is missing at $INSTALL_DIR/"
    fi
fi

# --- step 2: shell completions --------------------------------------------
install_completions() {
    [ "$INSTALL_COMPLETIONS" = "1" ] || { say "skipping completions (--no-completions)"; return; }
    local crabcc_path="$INSTALL_DIR/$BIN_NAME"
    [ -x "$crabcc_path" ] || crabcc_path="$(command -v "$BIN_NAME")"
    [ -x "$crabcc_path" ] || { warn "cannot find $BIN_NAME for completions; skipping"; return; }

    local shell_name
    case "${SHELL:-}" in
        */zsh)  shell_name="zsh" ;;
        */bash) shell_name="bash" ;;
        */fish) shell_name="fish" ;;
        *)      warn "unknown shell ${SHELL:-?}; skipping completions"; return ;;
    esac

    case "$shell_name" in
        zsh)
            local target=""
            for d in "${HOME}/.zsh/completions" /usr/local/share/zsh/site-functions /opt/homebrew/share/zsh/site-functions; do
                if [ -w "$d" ] || mkdir -p "$d" 2>/dev/null; then
                    target="$d"; break
                fi
            done
            [ -n "$target" ] || { warn "no writable zsh fpath dir; skipping"; return; }
            "$crabcc_path" completions zsh > "$target/_${BIN_NAME}"
            say "✓ zsh completion → $target/_${BIN_NAME}"
            warn "    add this to ~/.zshrc if it isn't already:"
            warn "        fpath=($target \$fpath); autoload -U compinit && compinit"
            ;;
        bash)
            local target="${HOME}/.local/share/bash-completion/completions"
            mkdir -p "$target"
            "$crabcc_path" completions bash > "$target/$BIN_NAME"
            say "✓ bash completion → $target/$BIN_NAME"
            ;;
        fish)
            local target="${HOME}/.config/fish/completions"
            mkdir -p "$target"
            "$crabcc_path" completions fish > "$target/$BIN_NAME.fish"
            say "✓ fish completion → $target/$BIN_NAME.fish"
            ;;
    esac
}
install_completions

# --- step 3: claude integration -------------------------------------------
install_claude_integration() {
    [ "$INSTALL_CLAUDE" = "1" ] || { say "skipping Claude integration (--no-claude)"; return; }
    if ! command -v claude >/dev/null 2>&1; then
        warn "claude CLI not on PATH; skipping skill + slash-command symlinks"
        warn "    install Claude Code first: https://claude.ai/code"
        return
    fi
    local src="$TMP/src"
    mkdir -p "$HOME/.claude/skills/crabcc" "$HOME/.claude/commands"
    if [ -f "$src/skill/crabcc/SKILL.md" ]; then
        ln -sf "$src/skill/crabcc/SKILL.md" "$HOME/.claude/skills/crabcc/SKILL.md"
        say "✓ skill linked → ~/.claude/skills/crabcc/SKILL.md"
    fi
    if [ -d "$src/commands" ]; then
        for f in "$src/commands"/*.md; do
            [ -e "$f" ] || continue
            ln -sf "$f" "$HOME/.claude/commands/$(basename "$f")"
        done
        say "✓ slash commands linked → ~/.claude/commands/"
    fi
    say "→ to register the MCP server:"
    say "    claude mcp add crabcc -- crabcc mcp"
}
install_claude_integration

# --- summary --------------------------------------------------------------
say "${BOLD}${GREEN}crabcc installed.${RESET}"
"$BIN_NAME" --version 2>/dev/null || true
cat <<EOF

${BOLD}next:${RESET}
    cd <your-repo>
    ${BLUE}crabcc go${RESET}             # one-shot: index + graph + memory + claude --effort max
                            (the default starting point — try this first)
    ${BLUE}crabcc sym Foo${RESET}        # find a definition
    ${BLUE}crabcc callers Foo${RESET}    # find call sites
    ${BLUE}crabcc memory search "..."${RESET}  # hybrid (BM25 + vector) recall
    ${BLUE}crabcc info${RESET}           # build provenance
    ${BLUE}crabcc upgrade${RESET}        # check for updates

docs: https://github.com/$REPO
EOF
