#!/usr/bin/env bash
# scripts/setup-gpg-signing.sh
#
# Configure GPG-signed commits + tags for THIS repository (not user-global,
# so we don't pollute other repos). Generates an ed25519 signing-only key
# and points `commit.gpgsign` / `tag.gpgsign` at it.
#
# Why ed25519: smaller, faster than RSA, GitHub-supported since 2019.
# Why repo-scoped: avoids changing global `user.signingkey` for users
# who maintain different keys per project.
#
# Idempotent:
#   - If a usable signing key already exists for $UID, reuse it.
#   - If `commit.gpgsign` is already true, skip the git config writes.
#   - Public-key export to clipboard happens every run (cheap, useful).
#
# Usage:
#   scripts/setup-gpg-signing.sh                 # generate or reuse, configure, export
#   scripts/setup-gpg-signing.sh --rotate        # force a fresh key (old key NOT deleted)
#   scripts/setup-gpg-signing.sh --print         # dry-run: print what would be done
#   scripts/setup-gpg-signing.sh --uninstall     # remove repo-local signing config
#   GIT_USER_NAME="..." GIT_USER_EMAIL="..." scripts/setup-gpg-signing.sh
#                                                # override identity (defaults to repo's
#                                                # configured user.name + user.email)

set -euo pipefail

ROTATE=0
PRINT=0
UNINSTALL=0
for arg in "$@"; do
  case "$arg" in
    --rotate)    ROTATE=1 ;;
    --print)     PRINT=1 ;;
    --uninstall) UNINSTALL=1 ;;
    -h|--help)
      sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      printf 'unknown flag: %s\n' "$arg" >&2
      exit 2
      ;;
  esac
done

# Resolve repo root (so this script works regardless of cwd within the repo).
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$REPO_ROOT" ]]; then
  printf 'not inside a git repository — run from any path under the repo\n' >&2
  exit 1
fi

run() {
  if [[ "$PRINT" -eq 1 ]]; then
    printf '+ %s\n' "$*"
  else
    "$@"
  fi
}

# ---------- uninstall path ----------
if [[ "$UNINSTALL" -eq 1 ]]; then
  printf 'Removing repo-local GPG signing config from %s\n' "$REPO_ROOT"
  for k in user.signingkey commit.gpgsign tag.gpgsign; do
    if git -C "$REPO_ROOT" config --local --get "$k" >/dev/null 2>&1; then
      run git -C "$REPO_ROOT" config --local --unset "$k"
      printf '  unset %s\n' "$k"
    fi
  done
  printf 'GPG key in your keyring is NOT touched. Remove via: gpg --delete-secret-keys <FPR>\n'
  exit 0
fi

# ---------- preflight ----------
if ! command -v gpg >/dev/null 2>&1; then
  printf 'gpg not found in PATH. Install with: brew install gnupg\n' >&2
  exit 1
fi

# Resolve identity for the new key. Prefer env override, then repo config.
NAME="${GIT_USER_NAME:-$(git -C "$REPO_ROOT" config --get user.name 2>/dev/null || true)}"
EMAIL="${GIT_USER_EMAIL:-$(git -C "$REPO_ROOT" config --get user.email 2>/dev/null || true)}"
if [[ -z "$NAME" || -z "$EMAIL" ]]; then
  printf 'No identity found. Set git user.name + user.email, or pass\n'      >&2
  printf 'GIT_USER_NAME and GIT_USER_EMAIL as env vars.\n'                    >&2
  exit 1
fi

# ---------- key discovery / generation ----------
# Match keys whose UID contains the email AND that are usable for signing
# (cap[S] in the capability flags from --with-colons).
existing_key() {
  gpg --list-secret-keys --with-colons 2>/dev/null \
    | awk -F: -v email="$EMAIL" '
        /^sec/ { fpr=""; usable = (index($12,"s")>0); next }
        /^fpr/ && fpr=="" { fpr=$10 }
        /^uid/ && index($10, email)>0 && usable && fpr!="" { print fpr; exit }
      '
}

KEY_FPR="$(existing_key || true)"

if [[ -n "$KEY_FPR" && "$ROTATE" -eq 0 ]]; then
  printf 'Reusing existing signing key for %s\n' "$EMAIL"
  printf '  fingerprint: %s\n' "$KEY_FPR"
else
  if [[ "$ROTATE" -eq 1 && -n "$KEY_FPR" ]]; then
    printf 'Rotating: leaving old key %s in keyring (delete manually if desired).\n' "$KEY_FPR"
  fi
  printf 'Generating ed25519 signing-only key for %s <%s> (2-year expiry)\n' "$NAME" "$EMAIL"
  run gpg --batch --quick-generate-key "$NAME <$EMAIL>" ed25519 sign 2y
  KEY_FPR="$(existing_key || true)"
  if [[ -z "$KEY_FPR" ]]; then
    printf 'key generation succeeded but no usable signing key found for %s\n' "$EMAIL" >&2
    exit 1
  fi
  printf '  fingerprint: %s\n' "$KEY_FPR"
fi

# Long key id is the last 16 hex chars of the fingerprint — what git wants.
LONG_KEY_ID="${KEY_FPR: -16}"
printf '  long key id: %s\n' "$LONG_KEY_ID"

# ---------- git configuration (repo-local only) ----------
configure_git() {
  local cur_key cur_commit cur_tag
  cur_key="$(git -C "$REPO_ROOT" config --local --get user.signingkey 2>/dev/null || true)"
  cur_commit="$(git -C "$REPO_ROOT" config --local --get commit.gpgsign 2>/dev/null || true)"
  cur_tag="$(git -C "$REPO_ROOT" config --local --get tag.gpgsign 2>/dev/null || true)"

  if [[ "$cur_key" != "$LONG_KEY_ID" ]]; then
    run git -C "$REPO_ROOT" config --local user.signingkey "$LONG_KEY_ID"
  fi
  if [[ "$cur_commit" != "true" ]]; then
    run git -C "$REPO_ROOT" config --local commit.gpgsign true
  fi
  if [[ "$cur_tag" != "true" ]]; then
    run git -C "$REPO_ROOT" config --local tag.gpgsign true
  fi
}

configure_git
printf 'Repo-local git config:\n'
printf '  user.signingkey  = %s\n' "$(git -C "$REPO_ROOT" config --local --get user.signingkey || echo '<unset>')"
printf '  commit.gpgsign   = %s\n' "$(git -C "$REPO_ROOT" config --local --get commit.gpgsign  || echo '<unset>')"
printf '  tag.gpgsign      = %s\n' "$(git -C "$REPO_ROOT" config --local --get tag.gpgsign     || echo '<unset>')"

# ---------- export public key + clipboard ----------
PUB_ARMOR="$(gpg --armor --export "$KEY_FPR" 2>/dev/null || true)"
if [[ -z "$PUB_ARMOR" ]]; then
  printf 'failed to export public key for %s\n' "$KEY_FPR" >&2
  exit 1
fi

CLIP=""
if   command -v pbcopy   >/dev/null 2>&1; then CLIP="pbcopy"
elif command -v wl-copy  >/dev/null 2>&1; then CLIP="wl-copy"
elif command -v xclip    >/dev/null 2>&1; then CLIP="xclip -selection clipboard"
elif command -v xsel     >/dev/null 2>&1; then CLIP="xsel --clipboard --input"
fi

if [[ -n "$CLIP" && "$PRINT" -eq 0 ]]; then
  printf '%s\n' "$PUB_ARMOR" | $CLIP
  printf '\nPublic key copied to clipboard via: %s\n' "$CLIP"
else
  printf '\nPublic key (paste into GitHub):\n\n%s\n' "$PUB_ARMOR"
fi

cat <<EOF

Next: paste the public key into
  https://github.com/settings/gpg/new

Verify with a signed commit:
  git -C "$REPO_ROOT" commit --allow-empty -m "test: gpg signing"
  git -C "$REPO_ROOT" log -1 --show-signature
EOF
