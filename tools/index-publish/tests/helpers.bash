#!/usr/bin/env bash
# tools/index-publish/tests/helpers.bash — shared bats helpers for build-manifest.

# Always resolves CRON_ROOT-style anchor via BASH_SOURCE so nested test dirs work.
TOOLS_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
export TOOLS_ROOT

setup_tempdir() {
  TMPD="$(mktemp -d)"
  export TMPD
}

teardown_tempdir() {
  rm -rf "$TMPD"
}

# Create a fake `crabcc` binary on PATH that echoes a controlled version.
# Usage: setup_fake_crabcc "4.0.0"
setup_fake_crabcc() {
  local version="${1:-4.0.0}"
  mkdir -p "$TMPD/bin"
  cat >"$TMPD/bin/crabcc" <<EOF
#!/usr/bin/env bash
case "\$1" in
  --version) echo "crabcc ${version}" ;;
esac
EOF
  chmod +x "$TMPD/bin/crabcc"
  export PATH="$TMPD/bin:$PATH"
}

# Create a fixture SQLite DB with controlled schema-version sources.
# Usage:
#   mk_db_pragma "$TMPD/fixture.db" 4         # PRAGMA user_version=4
#   mk_db_table  "$TMPD/fixture.db" 4         # schema_meta table with version=4
#   mk_db_empty  "$TMPD/fixture.db"           # neither
mk_db_pragma() {
  local db="$1" version="$2"
  sqlite3 "$db" "PRAGMA user_version=$version; CREATE TABLE noop(id INT);"
}

mk_db_table() {
  local db="$1" version="$2"
  sqlite3 "$db" <<SQL
PRAGMA user_version=0;
CREATE TABLE schema_meta(version INT);
INSERT INTO schema_meta VALUES($version);
SQL
}

mk_db_empty() {
  local db="$1"
  sqlite3 "$db" "CREATE TABLE noop(id INT);"
}

assert_jq() {
  local file="$1" filter="$2"
  jq -e "$filter" "$file" >/dev/null || {
    echo "assertion failed on $file: $filter"
    echo "content:"
    cat "$file"
    return 1
  }
}
