# crabcc-index publisher — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the publisher half of [`docs/superpowers/specs/2026-05-18-crabcc-index-publisher-design.md`](../specs/2026-05-18-crabcc-index-publisher-design.md). End state: on every push to `main` (rolling release) and every `v*` tag (durable release), GitHub Actions builds `.crabcc/index.db`, compresses with `zstd -19`, and uploads it together with a manifest JSON as release assets on a dedicated index release tag.

**Architecture:** A single GH Actions workflow orchestrates the run; non-trivial logic lives in a bats-tested bash script under `tools/index-publish/` so it can be exercised without invoking the workflow runner. Defensive schema-version detection from SQLite (PRAGMA user_version → schema_meta table → 0 sentinel). Manifest's `compat_policy` field documents that consumers gate on `crabcc_version` primarily.

**Tech Stack:** bash 5, jq, sqlite3, zstd (compression level 19), GitHub Actions, `gh` CLI, bats-core for the script unit tests, actionlint for workflow validation.

**Out of scope per spec §8:** `crabcc index --fetch` consumer (its own future spec), reusable `workflow_call` form (refactor follow-up after one release cycle), multi-target indexes, GC of old rolling-release assets, signed manifests.

---

## File Structure

```
.github/workflows/
├── index-publish.yml          (T2 — new — the workflow itself)
└── ci.yml                     (T2 — modify if actionlint not wired)

tools/index-publish/
├── README.md                  (T3 — new — operator notes + dry-run procedure)
├── build-manifest.sh          (T1 — new — manifest assembly, executable)
└── tests/
    ├── helpers.bash           (T1 — new — shared bats helpers + fixture builders)
    └── build-manifest.bats    (T1 — new — 5 unit tests)

README.md                       (T3 — modify — add workflow status badge)
Taskfile.yml                    (T1 — modify — add `index-publish-test` target)
```

`tools/index-publish/` is a NEW top-level directory parallel to `tools/crabcc-cron/`. It does not share helpers with crabcc-cron — each tool is independent.

---

## Task 1: `build-manifest.sh` + bats tests

**Rationale:** Pure shell-script logic, fully testable in isolation. The workflow (T2) is a thin wrapper around this script. Verify the schema-version detection branches (3 cases), the SHA256 + size computation, and the zstd compression actually reduces size.

**Files:**
- Create: `tools/index-publish/build-manifest.sh` (executable)
- Create: `tools/index-publish/tests/helpers.bash`
- Create: `tools/index-publish/tests/build-manifest.bats`
- Modify: `Taskfile.yml` (add `index-publish-test` target)

- [ ] **Step 1: Write the bats helpers**

`tools/index-publish/tests/helpers.bash`:

```bash
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
```

- [ ] **Step 2: Write the failing bats tests**

`tools/index-publish/tests/build-manifest.bats`:

```bash
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
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
mkdir -p tools/index-publish/tests
bats tools/index-publish/tests/build-manifest.bats
```

Expected: 5 fails with "build-manifest.sh: not found" or similar (script doesn't exist).

- [ ] **Step 4: Write `build-manifest.sh`**

`tools/index-publish/build-manifest.sh`:

```bash
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
```

- [ ] **Step 5: Make executable and run tests**

```bash
chmod +x tools/index-publish/build-manifest.sh
bats tools/index-publish/tests/build-manifest.bats
shellcheck -x tools/index-publish/build-manifest.sh
```

Expected: 5/5 pass; shellcheck clean.

- [ ] **Step 6: Add Taskfile targets**

Match the existing `taskfiles/cron.yml` pattern (this is the established convention in the repo — confirmed by the WL-2 + WL-3 work).

Create `taskfiles/index-publish.yml`:

```yaml
# taskfiles/index-publish.yml
version: '3'

tasks:
  test:
    desc: bats unit tests for tools/index-publish/build-manifest.sh
    cmds:
      - bats tools/index-publish/tests

  lint:
    desc: shellcheck tools/index-publish/build-manifest.sh
    cmds:
      - shellcheck -x tools/index-publish/build-manifest.sh
```

Open root `Taskfile.yml`. Find the existing `includes:` block. Add a new entry alongside the existing `cron:` entry (preserve the formatting style of the surrounding entries):

```yaml
includes:
  # ... existing entries ...
  cron: { taskfile: taskfiles/cron.yml, flatten: true }
  index-publish: { taskfile: taskfiles/index-publish.yml, flatten: true }
```

With `flatten: true`, the included tasks are reachable as `task test` / `task lint` (unscoped). That collides with the cron targets, which is why `cron.yml` uses `cron-test` / `cron-lint` as task names. Do the same here: use `index-publish-test` / `index-publish-lint` as the task names inside `taskfiles/index-publish.yml`.

Revised `taskfiles/index-publish.yml`:

```yaml
version: '3'

tasks:
  index-publish-test:
    desc: bats unit tests for tools/index-publish/build-manifest.sh
    cmds:
      - bats tools/index-publish/tests

  index-publish-lint:
    desc: shellcheck tools/index-publish/build-manifest.sh
    cmds:
      - shellcheck -x tools/index-publish/build-manifest.sh
```

Verify:

```bash
task --list-all | grep index-publish
task index-publish-test
task index-publish-lint
```

- [ ] **Step 7: Commit and push**

```bash
git add tools/index-publish/build-manifest.sh \
        tools/index-publish/tests/helpers.bash \
        tools/index-publish/tests/build-manifest.bats \
        Taskfile.yml taskfiles/index-publish.yml
git -c commit.gpgsign=false commit -m "feat(index-publish): build-manifest.sh + bats unit tests"
git push
```

(If you used the inline-tasks variant in step 6, drop `taskfiles/index-publish.yml` from the `git add`.)

---

## Task 2: GitHub Actions workflow + actionlint wiring

**Rationale:** Thin YAML orchestrator that calls `build-manifest.sh`, applies the size guard, picks a release tag based on event, and uploads. Validate with `actionlint`. The first invocation will be a manual `workflow_dispatch` dry-run (see Final Verification).

**Files:**
- Create: `.github/workflows/index-publish.yml`
- Modify: `.github/workflows/ci.yml` (add actionlint step if missing)

- [ ] **Step 1: Read the existing CI workflow**

```bash
cat .github/workflows/ci.yml | grep -A2 actionlint
```

If you see an `actionlint` step or `rhysd/actionlint` reference: skip Step 4 of this task.
Otherwise: Step 4 wires it in.

- [ ] **Step 2: Write `.github/workflows/index-publish.yml`**

```yaml
name: index-publish

on:
  push:
    branches: [main]
    tags: ['v*']
  workflow_dispatch:
    inputs:
      dry_run:
        type: boolean
        default: false
        description: 'Build everything but skip the gh release upload step.'

permissions:
  contents: write  # required for gh release create / upload

jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Install sqlite3 + zstd
        run: |
          sudo apt-get update
          sudo apt-get install -y sqlite3 zstd

      - name: Rust cache
        uses: Swatinem/rust-cache@v2

      - name: Build crabcc binary
        run: cargo build --release -p crabcc-cli

      - name: Build crabcc index against this repo
        run: ./target/release/crabcc index

      - name: Build manifest + compress index
        run: |
          mkdir -p artifacts
          tools/index-publish/build-manifest.sh \
            --db .crabcc/index.db \
            --crabcc-binary ./target/release/crabcc \
            --source-repo "$GITHUB_REPOSITORY" \
            --source-sha "$GITHUB_SHA" \
            --output-dir artifacts

      - name: Size guard (max 100 MB compressed)
        run: |
          zst_file=$(ls artifacts/crabcc-index-*.db.zst)
          size=$(stat -c %s "$zst_file")
          if (( size > 100000000 )); then
            echo "::error::compressed index size $size bytes exceeds 100 MB cap"
            exit 1
          fi
          echo "compressed size: $size bytes (under 100 MB cap)"

      - name: Determine release tag
        id: tag
        run: |
          version=$(grep -E '^version\s*=' Cargo.toml | head -1 | sed -E 's/.*"([^"]+)".*/\1/')
          if [[ "$GITHUB_REF_TYPE" == "tag" ]]; then
            tag="${version}-index"
          elif [[ "$GITHUB_EVENT_NAME" == "workflow_dispatch" ]]; then
            tag="${version}-index-manual-${GITHUB_RUN_ID}"
          else
            tag="${version}-index-latest"
          fi
          echo "tag=$tag" >> "$GITHUB_OUTPUT"
          echo "version=$version" >> "$GITHUB_OUTPUT"

      - name: Step summary
        run: |
          manifest_path=$(ls artifacts/crabcc-index-*.manifest.json)
          {
            echo "## crabcc-index publish"
            echo ""
            echo "- **Release tag:** \`${{ steps.tag.outputs.tag }}\`"
            echo "- **Source SHA:** \`$GITHUB_SHA\`"
            echo "- **Event:** \`$GITHUB_EVENT_NAME\`"
            echo "- **Dry run:** \`${{ inputs.dry_run }}\`"
            echo ""
            echo '### Manifest'
            echo ''
            echo '```json'
            cat "$manifest_path"
            echo ''
            echo '```'
          } >> "$GITHUB_STEP_SUMMARY"

      - name: Upload release (skipped on dry-run)
        if: ${{ inputs.dry_run != true }}
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: |
          tag="${{ steps.tag.outputs.tag }}"
          # Create the release if it doesn't exist. Tolerate "already exists".
          if ! gh release view "$tag" >/dev/null 2>&1; then
            gh release create "$tag" \
              --target "$GITHUB_SHA" \
              --title "crabcc index $tag" \
              --notes "crabcc-index built from commit $GITHUB_SHA. See attached manifest for details."
          fi
          gh release upload "$tag" \
            artifacts/crabcc-index-*.db.zst \
            artifacts/crabcc-index-*.manifest.json \
            --clobber

      - name: Dry-run notice
        if: ${{ inputs.dry_run == true }}
        run: |
          echo "DRY-RUN: would publish to ${{ steps.tag.outputs.tag }} but skipped gh release upload."
```

- [ ] **Step 3: Validate the workflow YAML**

```bash
if command -v actionlint >/dev/null 2>&1; then
  actionlint .github/workflows/index-publish.yml
else
  echo "(actionlint not installed locally; CI's actionlint step from T2 step 4 will catch errors on push)"
fi
```

If `actionlint` reports errors, fix them. Common gotchas:
- Deprecated action versions (e.g. `actions/checkout@v3` → bump to `@v4`).
- Undefined contexts: `inputs.dry_run` on `push` events is intentionally referenced. GH treats it as empty/undefined on `push`, so `${{ inputs.dry_run != true }}` evaluates to `true` (upload happens) on push, and `${{ inputs.dry_run == true }}` evaluates to `false` (dry-run notice doesn't fire). Actionlint may emit a warning about this — accept it.
- The `permissions: contents: write` at the workflow level is required for `gh release create`/`upload`; don't remove.

- [ ] **Step 4 (conditional): Add actionlint to `ci.yml`**

Skip this step if Step 1 found actionlint already wired. Otherwise:

Find an existing `lint` or `check` job in `ci.yml`. Add a step at the end of it:

```yaml
      - name: actionlint
        uses: raven-actions/actionlint@v2
        with:
          matcher: true
```

If there's no lint job, add this minimal one to the workflow:

```yaml
  actionlint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: actionlint
        uses: raven-actions/actionlint@v2
        with:
          matcher: true
```

- [ ] **Step 5: Commit and push**

```bash
git add .github/workflows/index-publish.yml
# Include ci.yml only if step 4 modified it:
git status .github/workflows/ci.yml | grep -q modified && git add .github/workflows/ci.yml
git -c commit.gpgsign=false commit -m "feat(index-publish): GH Actions workflow for index release"
git push
```

---

## Task 3: Operator docs + README badge

**Rationale:** Document the workflow and the dry-run procedure so the first manual invocation (and any future re-publish) has a checklist. Add a status badge so the publisher's health is visible on the main README.

**Files:**
- Create: `tools/index-publish/README.md`
- Modify: `README.md` (add workflow badge)

- [ ] **Step 1: Write `tools/index-publish/README.md`**

```markdown
# index-publish

Publishes `.crabcc/index.db` as a versioned GitHub release asset on every
push to `main` (rolling release) and every `v*` tag (durable release).
See [`docs/superpowers/specs/2026-05-18-crabcc-index-publisher-design.md`](../../docs/superpowers/specs/2026-05-18-crabcc-index-publisher-design.md) for the design.

## Artifacts

Each release contains two files:

- `crabcc-index-<crabcc_version>.db.zst` — the index, compressed with `zstd -19`.
- `crabcc-index-<crabcc_version>.manifest.json` — metadata describing the build.

## Release tags

| Event | Release tag |
|---|---|
| Push to `main` | `<version>-index-latest` (force-updated; same shape as nightly-linux.yml) |
| Push of `v*` tag | `<version>-index` (durable, immutable) |
| Manual `workflow_dispatch` | `<version>-index-manual-<run_id>` |

The `<version>` value comes from `Cargo.toml`'s workspace `version`.

## Local development

\`\`\`bash
# Lint
task index-publish-lint

# Unit tests for the manifest assembly script
task index-publish-test

# Manual end-to-end run (requires sqlite3 + zstd + a built crabcc):
cargo build --release -p crabcc-cli
./target/release/crabcc index
mkdir -p /tmp/index-out
tools/index-publish/build-manifest.sh \\
  --db .crabcc/index.db \\
  --crabcc-binary ./target/release/crabcc \\
  --source-repo peterlodri-sec/crabcc \\
  --source-sha "$(git rev-parse HEAD)" \\
  --output-dir /tmp/index-out
ls -la /tmp/index-out/
cat /tmp/index-out/crabcc-index-*.manifest.json | jq .
\`\`\`

## First-time deployment / verification

The workflow is wired but should be exercised in dry-run mode before relying on
the automated triggers.

1. Open the **Actions** tab on GitHub → **index-publish** workflow.
2. Click **Run workflow** → set **dry_run** to `true` → run on `main`.
3. Verify the run succeeds and the step summary shows a sensible manifest.
4. Re-run with **dry_run** `false` to produce the first real artifact.
5. Confirm the new release is visible under **Releases** with both `.db.zst`
   and `.manifest.json` attached.
6. From then on, every push to `main` and every `v*` tag publishes automatically.

## Disabling / unwinding

Temporary disable: disable the workflow from the Actions tab (top-right menu).
Permanent removal: `git rm .github/workflows/index-publish.yml`.
Existing release artifacts are not deleted by either action — manual cleanup
via `gh release delete <tag>` if you want them gone.

## Out of scope (deferred follow-up plans)

- `crabcc index --fetch <owner/repo>` consumer (own future spec).
- Reusable workflow (`on: workflow_call`) callable from sibling repos.
- Multi-target indexes, GC of old rolling-release assets, signed manifests.

See the design spec for the full deferred list.
```

(Note: in the local-development code block above, replace the doubled
backslashes with normal line-continuation backslashes when you actually
write the file — they're escaped here only because the plan is embedded
in a markdown code fence.)

- [ ] **Step 2: Add workflow badge to main `README.md`**

Read the current top of `README.md`:

```bash
head -10 README.md
```

Look for existing badge block (`![ci](...)`, `![nightly](...)`, etc.). Add the index-publish badge alongside them, near the top:

```markdown
![index-publish](https://github.com/peterlodri-sec/crabcc/actions/workflows/index-publish.yml/badge.svg)
```

If no badge block exists, insert one after the project title line. Match the formatting of any nearby badges in the repo's other markdown if they exist.

- [ ] **Step 3: Commit and push**

```bash
git add tools/index-publish/README.md README.md
git -c commit.gpgsign=false commit -m "docs(index-publish): operator README + workflow badge"
git push
```

---

## Final verification

After all 3 tasks land on the branch:

- [ ] **Bats unit tests:**

```bash
task index-publish-test
```

Expected: 5/5 pass.

- [ ] **Shellcheck:**

```bash
task index-publish-lint
```

Expected: clean.

- [ ] **YAML lint:**

```bash
actionlint .github/workflows/index-publish.yml
```

Expected: clean (assumes `actionlint` is installed locally; the CI job from T2 step 4 will catch it on push).

- [ ] **Full repo CI (`task ci`)** — ensure no regression to existing test suites.

- [ ] **Open PR** against `main` and let CI run. The index-publish workflow itself will NOT fire on the PR branch (only on `main` and tags) — that's intentional. Validation happens in two waves:
  1. PR merges → workflow fires on `main` push → uploads to `v<version>-index-latest`.
  2. From the Actions UI, manually re-run the workflow with `dry_run: true` to verify the dry-run path also works end-to-end.

- [ ] **Spec coverage check** — map every spec section to a task:

  | Spec section | Task |
  |---|---|
  | §3.1 Trigger matrix (push main, push v*, workflow_dispatch) | T2 (workflow `on:` block) |
  | §3.2 Filesystem layout | T1 (tools/index-publish), T2 (.github/workflows) |
  | §3.3 Per-run steps 1–10 | T2 (workflow YAML implements all 10) |
  | §3.4 What build-manifest.sh does (5 sub-steps) | T1 (script implements all 5) |
  | §4 Schema version handling (defensive PRAGMA → schema_meta → 0) | T1 (covered by tests 1, 2, 3) |
  | §5.1 Manifest shape | T1 (jq construction) + tests 1, 2 verify fields |
  | §5.2 Asset naming | T1 (path template), T2 (uploaded as-is) |
  | §5.3 Consumer expectations (documentation only) | Embedded in spec, not implemented |
  | §6 Failure modes | T2 (size guard, gh upload with --clobber, dry-run) |
  | §7.1 Three testing layers | T1 (bats), T2 (actionlint via ci.yml), Final Verification (manual dry-run) |
  | §7.2 5 bats tests | T1 step 2 (all 5 written verbatim from spec) |
  | §7.3 Dry-run gate | T2 (workflow_dispatch input + skip-upload conditional) |
  | §8 Out of scope | Documented in plan header + T3 README; no tasks |
  | §9 Implementation order | This plan's task ordering matches spec's |
  | §10 Open implementation questions | T1's bats test 3 resolves the "what if both sources empty?" question by exercising it |

- [ ] **Open follow-up plan stubs** for the deferred items:
  - `docs/superpowers/plans/<later>-crabcc-index-consumer.md` — the `crabcc index --fetch <owner/repo>` subcommand.
  - `docs/superpowers/plans/<later>-index-publish-reusable-workflow.md` — refactor to `on: workflow_call` after one release cycle.

  Stubs are one-line specs for now; flesh out when prioritized.
