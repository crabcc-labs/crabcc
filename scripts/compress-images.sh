#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# crabcc :: scripts/compress-images.sh
#
# Lossless image compression for repo assets (UI mockups, design refs,
# screenshots). Default mode: shrink without ever degrading quality.
#
# Usage:
#   scripts/compress-images.sh path/to/img.png [more...]
#   scripts/compress-images.sh --staged    # operate on `git diff --cached`
#   scripts/compress-images.sh --since=HEAD~5 path/to/dir   # since-rev mode
#
# Tools (install with `brew install oxipng jpegoptim`):
#   * oxipng    — lossless PNG (Rust, fast, safe defaults)
#   * jpegoptim — lossless JPEG (Huffman re-pack + metadata strip)
#
# Both are optional; missing tools downgrade to a warning, not an error,
# so the hook never blocks a commit just because an env is incomplete.
#
# Skips files smaller than COMPRESS_MIN_BYTES (default 10240 = 10 KiB) —
# tiny icons rarely shrink and the per-file fork cost dominates.
#
# Bypass: `CRABCC_SKIP_COMPRESS=1` in the env.
# ---------------------------------------------------------------------------

set -uo pipefail

if [ "${CRABCC_SKIP_COMPRESS:-0}" = "1" ]; then
    echo "[compress-images] CRABCC_SKIP_COMPRESS=1 — skipping"
    exit 0
fi

MIN_BYTES="${COMPRESS_MIN_BYTES:-10240}"

have() { command -v "$1" >/dev/null 2>&1; }

# Portable file-size helper: BSD stat (mac) vs GNU stat (linux).
filesize() {
    if stat -f%z "$1" >/dev/null 2>&1; then
        stat -f%z "$1"
    else
        stat -c%s "$1"
    fi
}

png_compress() {
    if have oxipng; then
        # -o 2: balance compression vs speed; safe default.
        # --strip safe: drops ancillary chunks that don't affect rendering
        # (timestamps, comments) but keeps gAMA, sRGB, iCCP — so colors
        # don't shift on color-managed displays.
        oxipng -o 2 --strip safe --quiet "$1"
    else
        echo "  ⚠ oxipng not installed (brew install oxipng) — skipping $1" >&2
        return 1
    fi
}

jpg_compress() {
    if have jpegoptim; then
        # --strip-all: drops EXIF / IPTC / XMP / comments; mockups don't
        # need GPS or camera metadata. Lossless re-encoding only.
        jpegoptim --strip-all --quiet "$1"
    else
        echo "  ⚠ jpegoptim not installed (brew install jpegoptim) — skipping $1" >&2
        return 1
    fi
}

# ----- input collection -----

files=()
if [ "${1:-}" = "--staged" ]; then
    while IFS= read -r f; do
        case "$f" in
            *.png|*.PNG|*.jpg|*.JPG|*.jpeg|*.JPEG) files+=("$f") ;;
        esac
    done < <(git diff --cached --name-only --diff-filter=ACM)
elif [[ "${1:-}" == --since=* ]]; then
    rev="${1#--since=}"
    shift
    paths=("$@")
    while IFS= read -r f; do
        case "$f" in
            *.png|*.PNG|*.jpg|*.JPG|*.jpeg|*.JPEG) files+=("$f") ;;
        esac
    done < <(git diff --name-only --diff-filter=ACM "$rev" -- "${paths[@]}")
else
    files=("$@")
fi

[ ${#files[@]} -eq 0 ] && exit 0

# ----- compression loop -----

total_before=0
total_after=0
processed=0
for f in "${files[@]}"; do
    [ -f "$f" ] || continue

    before=$(filesize "$f")
    if [ "$before" -lt "$MIN_BYTES" ]; then
        continue
    fi

    case "$f" in
        *.png|*.PNG) png_compress "$f" || continue ;;
        *.jpg|*.JPG|*.jpeg|*.JPEG) jpg_compress "$f" || continue ;;
        *) continue ;;
    esac

    after=$(filesize "$f")
    if [ "$after" -ge "$before" ]; then
        # Already optimal — no shrinkage. Don't print noise.
        continue
    fi
    pct=$(( (before - after) * 100 / before ))
    printf "  ✓ %s  %s → %s bytes (-%s%%)\n" "$f" "$before" "$after" "$pct"
    total_before=$((total_before + before))
    total_after=$((total_after + after))
    processed=$((processed + 1))
done

if [ "$processed" -gt 0 ]; then
    saved=$((total_before - total_after))
    pct=$(( saved * 100 / total_before ))
    printf "[compress-images] %s file(s) shrunk by %s bytes (-%s%%)\n" \
        "$processed" "$saved" "$pct"
fi

exit 0
