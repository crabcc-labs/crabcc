#!/usr/bin/env bash
# scripts/fix-style.sh
#
# Apply Rust style fixes across the whole workspace with automatic
# per-file rollback when a substitution causes a compile error.
#
# Strategy:
#   1. Apply all patterns to all .rs files at once (fast sed/perl pass).
#   2. Run `cargo build --workspace` (uses incremental cache — fast).
#   3. On failure: extract the failing file paths from rustc error output,
#      git-checkout those files to roll them back, then retry.
#   4. Repeat up to MAX_ITERS times, then run `cargo fmt --all`.
#
# Usage:
#   bash scripts/fix-style.sh              # apply + fix + fmt
#   bash scripts/fix-style.sh --dry-run    # show what would change, no writes
#   bash scripts/fix-style.sh --patterns   # print all patterns and exit
#
# Patterns applied (each safe enough to attempt; compile check is the safety net):
#   P01  .to_string_lossy().to_string()          → .to_string_lossy().into_owned()
#   P02  .unwrap_or("")                           → .unwrap_or_default()
#   P03  .unwrap_or(false)                        → .unwrap_or_default()
#   P04  .unwrap_or(0)                            → .unwrap_or_default()
#   P05  .map(|x| x.clone())  single-char var     → .map(Clone::clone)
#   P06  .map(|x| x.into())   single-char var     → .map(Into::into)
#   P07  .map(|x| x.to_string()) single-char var  → .map(str::to_string)  [&str only]
#   P08  .map(|x| x.is_file()).unwrap_or(false)   → .is_some_and(|x| x.is_file())
#   P09  x.len() == 0                             → x.is_empty()
#   P10  x.len() != 0  /  x.len() > 0            → !x.is_empty()

set -euo pipefail

WORKSPACE_ROOT="$(git rev-parse --show-toplevel)"
cd "$WORKSPACE_ROOT"

DRY_RUN=0
PRINT_PATTERNS=0
for arg in "$@"; do
  case "$arg" in
    --dry-run)       DRY_RUN=1 ;;
    --patterns)      PRINT_PATTERNS=1 ;;
  esac
done

if [[ "$PRINT_PATTERNS" == "1" ]]; then
  grep '# P[0-9]' "$0" | sed 's/^[[:space:]]*//'
  exit 0
fi

MAX_ITERS=5

# Crates that are known to be excluded from the normal build check
# (e.g. depend on a newer rustc toolchain than the workspace minimum).
# Keep in sync with Cargo.toml workspace.exclude.
EXCLUDE_CRATES=(crabcc-godfather)

# ── apply_patterns ────────────────────────────────────────────────────────────
# Apply all pattern substitutions to a single file in-place.
apply_patterns() {
  local f="$1"

  # P01: .to_string_lossy().to_string() → .to_string_lossy().into_owned()
  sed -i 's/\.to_string_lossy()\.to_string()/.to_string_lossy().into_owned()/g' "$f"

  # P02: .unwrap_or("") → .unwrap_or_default()
  sed -i 's/\.unwrap_or("")/.unwrap_or_default()/g' "$f"

  # P03: .unwrap_or(false) → .unwrap_or_default()
  sed -i 's/\.unwrap_or(false)/.unwrap_or_default()/g' "$f"

  # P04: .unwrap_or(0) → .unwrap_or_default()  (numeric types; rolled back if wrong)
  sed -i 's/\.unwrap_or(0)/.unwrap_or_default()/g' "$f"

  # P05: .map(|x| x.clone()) → .map(Clone::clone)  (single lowercase var)
  perl -i -p0e 's/\.map\(\|([a-z])\| \1\.clone\(\)\)/.map(Clone::clone)/g' "$f" 2>/dev/null || true

  # P06: .map(|x| x.into()) → .map(Into::into)  (rolled back for owned types)
  perl -i -p0e 's/\.map\(\|([a-z])\| \1\.into\(\)\)/.map(Into::into)/g' "$f" 2>/dev/null || true

  # P07: REMOVED — .map(|x| x.to_string()) is only valid as .map(str::to_string) when
  #   the iterator yields &str references. For owned types (Option<i64>, Option<u32>, …)
  #   the closure form is correct and mandatory. Type-unsafe to apply blindly.

  # P08: .map(|x| EXPR).unwrap_or(false) → .is_some_and(|x| EXPR)
  # Only handles single-line cases where the closure body fits on one line.
  perl -i -p0e 's/\.map\((\|[a-z]+\| [^)]+)\)\.unwrap_or\(false\)/.is_some_and($1)/g' "$f" 2>/dev/null || true

  # P09: .len() == 0 → .is_empty()
  sed -i 's/\.len() == 0/.is_empty()/g' "$f"

  # P10: .len() != 0 / .len() > 0 → !.is_empty()
  sed -i -E 's/\.len\(\) (!=|>) 0/!.is_empty()/g' "$f"
}

# ── collect files ─────────────────────────────────────────────────────────────
mapfile -t ALL_RS < <(
  git ls-files -- 'crates/*.rs' 'crates/**/*.rs' \
    | grep -v '/target/' \
    | sort
)

echo "==> Scanning ${#ALL_RS[@]} Rust files for style patterns..."

CHANGED_FILES=()

for f in "${ALL_RS[@]}"; do
  before=$(md5sum "$f" | cut -d' ' -f1)

  if [[ "$DRY_RUN" == "1" ]]; then
    tmp=$(mktemp)
    cp "$f" "$tmp"
    apply_patterns "$tmp"
    after=$(md5sum "$tmp" | cut -d' ' -f1)
    rm -f "$tmp"
  else
    apply_patterns "$f"
    after=$(md5sum "$f" | cut -d' ' -f1)
  fi

  if [[ "$before" != "$after" ]]; then
    CHANGED_FILES+=("$f")
    echo "  modified: $f"
  fi
done

echo ""
echo "==> ${#CHANGED_FILES[@]} files modified."

if [[ "$DRY_RUN" == "1" ]]; then
  echo "(dry-run — no files written)"
  exit 0
fi

if [[ "${#CHANGED_FILES[@]}" == "0" ]]; then
  echo "Nothing to do — codebase already clean."
  exit 0
fi

# ── build + rollback loop ─────────────────────────────────────────────────────
echo ""
echo "==> Running cargo build --workspace (iteration 1 of $MAX_ITERS)..."

for iter in $(seq 1 $MAX_ITERS); do
  # Build all workspace members. Exclude known toolchain-incompatible crates
  # via --exclude so their pre-existing errors don't mask our failures.
  EXCLUDE_FLAGS=()
  for ex in "${EXCLUDE_CRATES[@]}"; do
    EXCLUDE_FLAGS+=(--exclude "$ex")
  done
  BUILD_OUT=$(cargo build --workspace "${EXCLUDE_FLAGS[@]}" 2>&1) && {
    echo "✅ Build passed on iteration $iter."
    break
  }

  if [[ "$iter" == "$MAX_ITERS" ]]; then
    echo "❌ Build still failing after $MAX_ITERS iterations."
    echo "   Remaining errors:"
    printf '%s\n' "$BUILD_OUT" | grep '^error' | head -20
    echo ""
    echo "   Rolling back ALL remaining changes."
    git checkout -- "${CHANGED_FILES[@]}"
    exit 1
  fi

  echo "❌ Build failed on iteration $iter. Extracting bad files..."

  # Extract file paths from rustc error lines like: " --> crates/foo/src/bar.rs:12:3"
  # Uses portable sed/grep rather than grep -oP (not always available).
  mapfile -t BAD_FILES < <(
    printf '%s\n' "$BUILD_OUT" \
      | grep '^\s*-->' \
      | sed 's/^\s*-->\s*\([^:]*\.rs\):.*/\1/' \
      | sort -u
  )

  if [[ "${#BAD_FILES[@]}" == "0" ]]; then
    echo "   Cannot parse error locations — rolling back ALL changes."
    git checkout -- "${CHANGED_FILES[@]}"
    exit 1
  fi

  for bad in "${BAD_FILES[@]}"; do
    if [[ -f "$bad" ]]; then
      git checkout -- "$bad"
      echo "   rolled back: $bad"
      # Remove from CHANGED_FILES tracking
      CHANGED_FILES=("${CHANGED_FILES[@]/$bad}")
    fi
  done

  echo "==> Retrying build (iteration $((iter+1)) of $MAX_ITERS)..."
done

# ── final fmt ─────────────────────────────────────────────────────────────────
echo ""
echo "==> Running cargo fmt --all..."
cargo fmt --all

echo ""
echo "Done. Summary:"
echo "  Files successfully fixed : $(git diff --name-only | wc -l)"
echo "  Run 'git diff --stat' to review changes."
