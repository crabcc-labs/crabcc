#!/usr/bin/env bash
# tools/index-publish/build-manifest.sh
#
# Assemble the manifest + compressed index for the index-publish workflow.
# Pure CLI; produces files in the output dir, no side effects beyond that.
#
# Args (all required):
#   --db <path>              path to the raw .crabcc/index.db
#   --crabcc-binary <path>   crabcc binary to invoke for --version
#   --source-repo <owner/name>  repo slug for the manifest
#   --source-sha <git_sha>   git SHA for the manifest
#   --output-dir <path>      where to write the manifest + .db.zst
#
# Outputs (in --output-dir):
#   crabcc-index-<crabcc_version>.db.zst
#   crabcc-index-<crabcc_version>.manifest.json

set -euo pipefail

DB=""
CRABCC=""
SOURCE_REPO=""
SOURCE_SHA=""
OUTPUT_DIR=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --db) DB="$2"; shift 2 ;;
    --crabcc-binary) CRABCC="$2"; shift 2 ;;
    --source-repo) SOURCE_REPO="$2"; shift 2 ;;
    --source-sha) SOURCE_SHA="$2"; shift 2 ;;
    --output-dir) OUTPUT_DIR="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

[[ -f "$DB" ]] || { echo "db not found: $DB" >&2; exit 2; }
[[ -x "$CRABCC" ]] || { echo "crabcc not executable: $CRABCC" >&2; exit 2; }
[[ -n "$SOURCE_REPO" ]] || { echo "--source-repo required" >&2; exit 2; }
[[ -n "$SOURCE_SHA" ]] || { echo "--source-sha required" >&2; exit 2; }
[[ -n "$OUTPUT_DIR" ]] || { echo "--output-dir required" >&2; exit 2; }
mkdir -p "$OUTPUT_DIR"

# crabcc version
crabcc_version="$("$CRABCC" --version | awk '{print $2}')"
[[ -n "$crabcc_version" ]] || { echo "could not parse crabcc version" >&2; exit 2; }

# schema version (defensive: PRAGMA → schema_meta table → 0)
schema_version="$(sqlite3 "$DB" 'PRAGMA user_version' 2>/dev/null || echo 0)"
if [[ "$schema_version" == "0" ]]; then
  schema_version="$(sqlite3 "$DB" 'SELECT version FROM schema_meta LIMIT 1' 2>/dev/null || echo 0)"
fi

# raw db hash + size (Linux uses stat -c, macOS uses stat -f)
db_size_bytes="$(stat -c %s "$DB" 2>/dev/null || stat -f %z "$DB")"
db_sha256="$( (sha256sum "$DB" 2>/dev/null || shasum -a 256 "$DB") | awk '{print $1}' )"

# compress (-19 = best ratio; --keep preserves source for post-compress hashing)
zst_path="$OUTPUT_DIR/crabcc-index-${crabcc_version}.db.zst"
zstd -19 --keep -q -o "$zst_path" "$DB"

# zst hash + size
db_zst_size_bytes="$(stat -c %s "$zst_path" 2>/dev/null || stat -f %z "$zst_path")"
db_zst_sha256="$( (sha256sum "$zst_path" 2>/dev/null || shasum -a 256 "$zst_path") | awk '{print $1}' )"

# built_at: GNU date supports -Iseconds; macOS BSD date doesn't. Fallback.
built_at="$(date -u -Iseconds 2>/dev/null \
            || date -u '+%Y-%m-%dT%H:%M:%S+00:00')"

manifest_path="$OUTPUT_DIR/crabcc-index-${crabcc_version}.manifest.json"
jq -n \
  --arg cv "$crabcc_version" \
  --argjson sv "$schema_version" \
  --arg ba "$built_at" \
  --arg sr "$SOURCE_REPO" \
  --arg ss "$SOURCE_SHA" \
  --argjson dsb "$db_size_bytes" \
  --arg ds "$db_sha256" \
  --argjson zsb "$db_zst_size_bytes" \
  --arg zs "$db_zst_sha256" \
  '{
    crabcc_version: $cv,
    schema_version: $sv,
    compat_policy: "primary:crabcc_version;secondary:schema_version",
    built_at: $ba,
    source_repo: $sr,
    source_sha: $ss,
    db_size_bytes: $dsb,
    db_sha256: $ds,
    db_zst_size_bytes: $zsb,
    db_zst_sha256: $zs
  }' > "$manifest_path"

echo "wrote $manifest_path" >&2
echo "wrote $zst_path" >&2
