#!/usr/bin/env bats

load 'helpers'

setup() {
  setup_tempdir
  setup_fake_crabcc "4.0.0"
}
teardown() { teardown_tempdir; }

# helper: run build-manifest with the standard 5 args.
run_build_manifest() {
  local db="$1"
  "$TOOLS_ROOT/build-manifest.sh" \
    --db "$db" \
    --crabcc-binary "$(command -v crabcc)" \
    --source-repo "peterlodri-sec/crabcc" \
    --source-sha "0123456789abcdef0123456789abcdef01234567" \
    --output-dir "$TMPD/out"
}

@test "manifest: PRAGMA user_version=4 → schema_version=4" {
  mk_db_pragma "$TMPD/fixture.db" 4
  run_build_manifest "$TMPD/fixture.db"
  manifest="$TMPD/out/crabcc-index-4.0.0.manifest.json"
  assert_jq "$manifest" '.schema_version == 4'
  assert_jq "$manifest" '.crabcc_version == "4.0.0"'
  assert_jq "$manifest" '.compat_policy == "primary:crabcc_version;secondary:schema_version"'
}

@test "manifest: PRAGMA=0 + schema_meta table → schema_version from table" {
  mk_db_table "$TMPD/fixture.db" 4
  run_build_manifest "$TMPD/fixture.db"
  manifest="$TMPD/out/crabcc-index-4.0.0.manifest.json"
  assert_jq "$manifest" '.schema_version == 4'
}

@test "manifest: PRAGMA=0 + no schema_meta → schema_version=0 (sentinel)" {
  mk_db_empty "$TMPD/fixture.db"
  run_build_manifest "$TMPD/fixture.db"
  manifest="$TMPD/out/crabcc-index-4.0.0.manifest.json"
  assert_jq "$manifest" '.schema_version == 0'
  assert_jq "$manifest" '.compat_policy == "primary:crabcc_version;secondary:schema_version"'
}

@test "manifest: db_sha256 matches independent sha256sum" {
  mk_db_pragma "$TMPD/fixture.db" 4
  run_build_manifest "$TMPD/fixture.db"
  manifest="$TMPD/out/crabcc-index-4.0.0.manifest.json"
  expected=$(sha256sum "$TMPD/fixture.db" 2>/dev/null | awk '{print $1}' \
             || shasum -a 256 "$TMPD/fixture.db" | awk '{print $1}')
  manifest_sha=$(jq -r '.db_sha256' "$manifest")
  [[ "$manifest_sha" == "$expected" ]]
}

@test "manifest: zst is smaller than raw (zstd actually compressed)" {
  # Build a DB with enough content to be compressible.
  sqlite3 "$TMPD/fixture.db" <<SQL
PRAGMA user_version=4;
CREATE TABLE big(id INT, blob TEXT);
WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM c WHERE n < 1000)
INSERT INTO big SELECT n, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' FROM c;
SQL
  run_build_manifest "$TMPD/fixture.db"
  manifest="$TMPD/out/crabcc-index-4.0.0.manifest.json"
  db_size=$(jq -r '.db_size_bytes' "$manifest")
  zst_size=$(jq -r '.db_zst_size_bytes' "$manifest")
  [[ "$zst_size" -lt "$db_size" ]] || {
    echo "expected zst_size ($zst_size) < db_size ($db_size)"
    return 1
  }
}
