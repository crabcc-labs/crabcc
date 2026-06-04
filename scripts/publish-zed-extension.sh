#!/usr/bin/env bash
# scripts/publish-zed-extension.sh
#
# Publish the crabcc Zed extension: cut the workspace release tag AND mirror
# editors/zed/crabcc to the public crabcc-labs/zed-crabcc repo (the source
# the Zed extension registry builds from — crabcc itself is private).
#
# Two independent steps, runnable together (default) or one at a time:
#
#   tag     Create + push the annotated `v<VERSION>` tag at origin/<BRANCH>.
#           Pushing it triggers .github/workflows/release.yml, which builds
#           the release tarballs and publishes the GitHub release.
#
#   mirror  Create crabcc-labs/zed-crabcc (if absent) and push the extension
#           directory as a SINGLE clean commit — fresh history, authored by
#           YOUR git identity, so no monorepo (Claude-authored) commits and
#           no unrelated files come along.
#
# Why a script: the managed/web execution environment can't push tags or
# create org repos (scoped integration token), so these final steps run from
# a machine that has your credentials + the `gh` CLI.
#
# Usage:
#   bash scripts/publish-zed-extension.sh                 # tag + mirror
#   bash scripts/publish-zed-extension.sh tag             # release tag only
#   bash scripts/publish-zed-extension.sh mirror          # public repo only
#   bash scripts/publish-zed-extension.sh --dry-run       # print, do nothing
#   VERSION=5.3.0 bash scripts/publish-zed-extension.sh   # override version
#
# Env overrides (all optional):
#   VERSION      release version            (default: workspace.package version)
#   PUBLIC_REPO  owner/name of the mirror   (default: crabcc-labs/zed-crabcc)
#   EXT_DIR      extension source dir        (default: editors/zed/crabcc)
#   BRANCH       branch to tag from          (default: main)
#   REMOTE       monorepo git remote         (default: origin)
set -euo pipefail

# --- config ------------------------------------------------------------------
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

EXT_DIR="${EXT_DIR:-editors/zed/crabcc}"
PUBLIC_REPO="${PUBLIC_REPO:-crabcc-labs/zed-crabcc}"
BRANCH="${BRANCH:-main}"
REMOTE="${REMOTE:-origin}"
# Default VERSION = the first top-level `version = "x.y.z"` in the root
# Cargo.toml (the [workspace.package] line).
VERSION="${VERSION:-$(grep -m1 '^version = "' Cargo.toml | cut -d'"' -f2)}"

DRY_RUN=0
CMD="all"

# --- helpers -----------------------------------------------------------------
log()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarn:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
run()  { if [ "$DRY_RUN" = 1 ]; then printf '\033[2m[dry-run] %s\033[0m\n' "$*"; else eval "$@"; fi; }

confirm() {
  [ "$DRY_RUN" = 1 ] && return 0
  read -r -p "$1 [y/N] " ans
  [[ "$ans" =~ ^[Yy]$ ]] || die "aborted by user"
}

# Lightweight guard against leaking secrets/env/private artifacts into the
# public repo. Mirrors the manual pre-publish audit.
audit_ext_dir() {
  log "Auditing $EXT_DIR for secrets / env files before publishing"
  local hits
  if hits="$(find "$EXT_DIR" -type f \( -name '.env*' -o -name '*.pem' -o -name '*.key' \) \
              -not -path '*/target/*' 2>/dev/null)" && [ -n "$hits" ]; then
    die "refusing to publish — found env/key files:"$'\n'"$hits"
  fi
  if grep -rniE 'ghp_[A-Za-z0-9]{20,}|github_pat_|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY-----' \
       "$EXT_DIR" --exclude-dir=target >/dev/null 2>&1; then
    die "refusing to publish — possible secret/token detected in $EXT_DIR"
  fi
  log "Audit clean."
}

# --- arg parsing -------------------------------------------------------------
for arg in "$@"; do
  case "$arg" in
    tag|mirror|all) CMD="$arg" ;;
    --dry-run)      DRY_RUN=1 ;;
    -h|--help)      sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *)              die "unknown argument: $arg (try --help)" ;;
  esac
done

[ -d "$EXT_DIR" ] || die "extension dir not found: $EXT_DIR (run from a crabcc checkout)"
[ -n "$VERSION" ] || die "could not determine VERSION (set VERSION=x.y.z)"

# --- step: tag ---------------------------------------------------------------
do_tag() {
  local tag="v$VERSION"
  log "Release tag: $tag  (from $REMOTE/$BRANCH)"

  run "git fetch --quiet $REMOTE $BRANCH --tags"

  if git rev-parse -q --verify "refs/tags/$tag" >/dev/null 2>&1 \
     || git ls-remote --tags "$REMOTE" "$tag" | grep -q "$tag"; then
    warn "tag $tag already exists locally or on $REMOTE — skipping tag creation"
    return 0
  fi

  # Sanity: the version we're tagging must match what's committed on the branch.
  local committed
  committed="$(git show "$REMOTE/$BRANCH:Cargo.toml" | grep -m1 '^version = "' | cut -d'"' -f2)"
  [ "$committed" = "$VERSION" ] \
    || die "version mismatch: tagging $VERSION but $REMOTE/$BRANCH Cargo.toml is $committed"

  confirm "Create and push annotated tag $tag at $REMOTE/$BRANCH (triggers release.yml)?"
  run "git tag -a '$tag' '$REMOTE/$BRANCH' -m '$tag'"
  run "git push '$REMOTE' '$tag'"
  log "Pushed $tag — release.yml will build tarballs and publish the GitHub release."
}

# --- step: mirror ------------------------------------------------------------
do_mirror() {
  log "Public mirror: $PUBLIC_REPO  (from $EXT_DIR)"

  # Read-only audit first — safe to run even in --dry-run.
  audit_ext_dir

  # Preflight guards: hard errors for a real run, warnings under --dry-run so
  # the full plan still prints.
  if ! command -v gh >/dev/null 2>&1; then
    [ "$DRY_RUN" = 1 ] && warn "the GitHub CLI ('gh') is not installed (required for a real run)" \
                       || die "the GitHub CLI ('gh') is required for mirroring"
  fi
  # Guard against accidentally committing as the bot identity.
  local who
  who="$(git config user.email || true)"
  if [ "$who" = "noreply@anthropic.com" ]; then
    [ "$DRY_RUN" = 1 ] && warn "git user.email is the bot identity ($who) — set your own before a real run" \
                       || die "git user.email is the bot identity ($who) — set your own before mirroring \
so the public repo has no Claude-authored commits"
  fi

  if gh repo view "$PUBLIC_REPO" >/dev/null 2>&1; then
    warn "$PUBLIC_REPO already exists — will push to it (force a clean tree if you intend a full resync)"
  else
    confirm "Create PUBLIC repo $PUBLIC_REPO?"
    run "gh repo create '$PUBLIC_REPO' --public \
      -d 'crabcc for Zed — ucracc-lsp wired into Zed. Mirror of crabcc:$EXT_DIR. GPLv3.'"
  fi

  # Build a fresh, single-commit tree in a temp dir (no monorepo history).
  local tmp
  tmp="$(mktemp -d)"
  log "Staging clean tree in $tmp"
  run "cp -R '$EXT_DIR/.' '$tmp/'"
  if [ "$DRY_RUN" = 1 ]; then
    log "[dry-run] would: git init + commit + push $PUBLIC_REPO main"
    rm -rf "$tmp"; return 0
  fi
  (
    cd "$tmp"
    git init -q
    git checkout -q -b main
    git add -A
    git commit -q -m "Initial public release of the crabcc Zed extension (GPLv3)"
    git remote add origin "git@github.com:$PUBLIC_REPO.git"
    git push -u origin main
  )
  rm -rf "$tmp"
  log "Mirrored $EXT_DIR → $PUBLIC_REPO (single clean commit on main)."
  warn "Auto-download note: lib.rs pulls ucracc-lsp binaries from $PUBLIC_REPO *releases* — \
publish per-platform tarballs there (or mirror them from the crabcc release) to light up tier-3."
}

# --- main --------------------------------------------------------------------
log "crabcc Zed-extension publisher  (VERSION=$VERSION, DRY_RUN=$DRY_RUN)"
case "$CMD" in
  tag)    do_tag ;;
  mirror) do_mirror ;;
  all)    do_tag; echo; do_mirror ;;
esac
log "Done."
