# crabcc-index publisher — release-artifact distribution of `.crabcc/index.db`

Date: 2026-05-18
Status: Design approved, ready for implementation plan
Scope: publisher only — consumer (`crabcc index --fetch`) is a separate
  follow-up spec.

## 1. Motivation

`.crabcc/index.db` is built locally per worktree. Every cron workload
(WL-3 today, WL-1 tomorrow, the morning digest after that) and every
fresh-clone consumer rebuilds the index from scratch — a few seconds to
a few minutes per repo. Publishing the index as a GitHub release asset
on each merge to `main` (and on every version tag) lets future
consumers download a pre-built, verifiable index instead of rebuilding.

This spec covers the publisher only. The consumer (`crabcc index
--fetch <owner/repo>` subcommand) gets its own spec after we have
real artifacts produced by this workflow for a release cycle. Shipping
the publisher first lets us validate artifact size + CI cost before
committing to a CLI surface.

## 2. Non-goals

- No `crabcc index --fetch` subcommand. Future.
- No cross-repo reusable workflow yet. After we dogfood this in
  `crabcc` for a release cycle, we'll refactor into a `workflow_call`
  reusable workflow as a follow-up.
- No `apt`-style package repository or per-repo index pinning. Each
  release ships its own asset; consumers pick by version.
- No automatic GC of old rolling-release assets. GH releases are cheap;
  manual deletion if it ever matters.
- No staleness alerting. If the workflow stops firing, the GH Actions
  badge in the README signals the failure.

## 3. Architecture

### 3.1 Trigger matrix

```
push to main         → release tag: v<version>-index-latest (force-updated)
push of tag v*       → release tag: v<version>-index       (durable, immutable)
workflow_dispatch    → optional manual invocation (used for first
                       smoke + emergency re-publish)
```

The `<version>` in the release tag comes from `Cargo.toml`'s top-level
`[workspace.package].version`, which already drives the binary release
workflow. This keeps the index release adjacent to the binary release.

### 3.2 Filesystem layout

```
.github/workflows/
└── index-publish.yml             (new — single workflow, three triggers)

tools/index-publish/
├── build-manifest.sh             (new — manifest assembly; testable in isolation)
└── tests/
    └── build-manifest.bats       (new — unit tests for the manifest script)
```

The workflow is a thin orchestrator (~80 lines YAML). All the
non-trivial shell logic lives in `tools/index-publish/build-manifest.sh`
so it's bats-testable.

### 3.3 Per-run steps (ubuntu-latest, single job)

1. `actions/checkout@v4` with `fetch-depth: 0` (needs full history for
   accurate index symbol resolution and a real `git rev-parse HEAD`).
2. `apt-get install -y sqlite3 zstd` (sqlite3 isn't always present;
   zstd is on modern ubuntu-latest but we install defensively).
3. `Swatinem/rust-cache@v2` (existing pattern from other workflows in
   this repo).
4. `cargo build --release -p crabcc-cli`.
5. `./target/release/crabcc index` against the repo (its own source).
6. Run `tools/index-publish/build-manifest.sh` against the produced
   `.crabcc/index.db` → emits `crabcc-index-<version>.manifest.json`
   and the size-checked, zstd-compressed
   `crabcc-index-<version>.db.zst` alongside.
7. Pre-upload size guard: if `db_zst_size_bytes > 100_000_000`, fail
   the run with a clear message. No silent truncation.
8. Determine release tag based on event:
   - `push` to `main`           → `${version}-index-latest`
   - `push` of `v*` tag         → `${version}-index`
   - `workflow_dispatch`        → `${version}-index-manual-${run_id}`
9. Upload via `gh release upload --clobber <tag> <files>`, creating the
   release with `gh release create --target <sha> --notes …` if it
   doesn't exist.
10. Emit a step summary (`>>$GITHUB_STEP_SUMMARY`) with crabcc version,
    schema version, source SHA, db sizes, release URL.

### 3.4 What `build-manifest.sh` does

A standalone bash script invoked as:

```
tools/index-publish/build-manifest.sh \
  --db .crabcc/index.db \
  --crabcc-binary ./target/release/crabcc \
  --source-repo "$GITHUB_REPOSITORY" \
  --source-sha  "$GITHUB_SHA" \
  --output-dir .
```

Steps:

1. Run the supplied crabcc binary with `--version` to get the
   `crabcc_version` string.
2. Read schema version defensively (see §4).
3. Compute `db_sha256` over the raw `.db`, `db_size_bytes` via
   `stat -c %s` (or `stat -f %z` on macOS for dev work).
4. Compress with `zstd -19 --keep` (the explicit `--keep` flag
   preserves the raw `.db` for the subsequent hash + size checks;
   default zstd behavior is also to keep the source, `--keep` is
   belt-and-braces). `-19` is the slowest/best compression; ~5x
   reduction on typical SQLite index; ~5–10s of CI per push.
5. Compute `db_zst_sha256` and `db_zst_size_bytes` on the compressed
   artifact.
6. Emit `crabcc-index-${crabcc_version}.manifest.json` with the schema
   in §5.1.

The `.db.zst` file is left in the output dir; the workflow uploads
both as release assets.

## 4. Schema version handling (defensive)

We don't trust a single source. The script tries each in order and
takes the first non-zero, non-empty result. Final fallback is `0`,
which the manifest documents as a sentinel meaning "unknown — use
crabcc_version for compatibility instead".

```bash
schema_version=$(sqlite3 "$DB" "PRAGMA user_version" 2>/dev/null || echo 0)
if [[ "$schema_version" == "0" ]]; then
  schema_version=$(sqlite3 "$DB" \
    "SELECT version FROM schema_meta LIMIT 1" 2>/dev/null || echo 0)
fi
```

Both queries are read-only. A missing pragma value is `0`. A missing
`schema_meta` table makes `sqlite3` exit non-zero, caught by the
`|| echo 0`. Worst case: `schema_version: 0` in the manifest — the
artifact still ships and is still usable.

`store.rs` may or may not actually set `PRAGMA user_version` today. The
implementation plan's first task verifies which sources are populated
on a real-world build of this repo's own index and documents the
result in the manifest's `compat_policy` field.

## 5. Artifact contract

### 5.1 Manifest shape

```json
{
  "crabcc_version": "4.0.0",
  "schema_version": 4,
  "compat_policy": "primary:crabcc_version;secondary:schema_version",
  "built_at": "2026-05-18T07:00:00Z",
  "source_repo": "peterlodri-sec/crabcc",
  "source_sha": "0f9887efd8ddd39fb645811ecbcae3c69ca15b94",
  "db_size_bytes": 12345678,
  "db_sha256": "<hex>",
  "db_zst_size_bytes": 2345678,
  "db_zst_sha256": "<hex>"
}
```

### 5.2 Asset naming

Per release:

- `crabcc-index-<crabcc_version>.db.zst` — the compressed index
- `crabcc-index-<crabcc_version>.manifest.json` — the metadata

Example: `crabcc-index-4.0.0.db.zst` + `crabcc-index-4.0.0.manifest.json`.

The crabcc version uniquely identifies the artifact within a release.
Releases themselves are tagged by the index-tag convention (`-index`
or `-index-latest`) which is separate from the source binary's release
tags (`v4.0.0`).

### 5.3 Consumer expectations (informational, locked in the consumer spec)

When the consumer ships, it will:

1. Fetch the manifest first.
2. Compare its own `crabcc --version` to manifest's `crabcc_version`.
   Mismatch → refuse, fall back to local rebuild.
3. Compare `schema_version` if both binary and manifest have one ≠ 0.
   Mismatch → refuse, fall back to local rebuild.
4. Download the `.db.zst`, verify `db_zst_sha256`, decompress, verify
   `db_sha256`, write to `.crabcc/index.db`.

This is documented here so the publisher's choices stay aligned even
while the consumer is still a future plan.

## 6. Failure modes & observability

The workflow is idempotent. Per-run failures don't corrupt the rolling
release.

| Failure | Behavior |
|---|---|
| `cargo build --release` fails | Workflow exits non-zero. Existing rolling release untouched. PR-merge CI is already gated on `cargo build`. |
| `crabcc index` fails | Workflow exits non-zero. Existing rolling release untouched. |
| `sqlite3` not on PATH | `apt-get install -y sqlite3` happens before use. |
| `zstd` not on PATH | `apt-get install -y zstd` happens before use. |
| `gh release upload` fails (transient API error) | `gh`'s built-in retries kick in (2 attempts, 30s backoff). Then fails; next push retries cleanly. |
| Existing `v<version>-index-latest` release doesn't exist | `gh release create … || gh release upload --clobber …` handles both cases. |
| Tag-driven run when `v<version>-index` already exists | Should never happen (tags are immutable). If it does (force-pushed tag), `--clobber` overwrites, with a warning logged. |
| Index `.db.zst` > 100 MB | Pre-upload check; workflow fails with a clear message. No silent truncation. |

**Observability:**

- Standard GH Actions logs.
- Each run writes a `$GITHUB_STEP_SUMMARY` block with crabcc version,
  schema version, source SHA, db sizes (raw + zst), release URL.
- A workflow status badge in `README.md`. Anyone scanning the README
  sees the publisher's health at a glance.

**No retry queue, no spool layer.** Next push retries naturally. CI is
the source of truth.

**No external alerting.** GH Actions' built-in email-on-failure is
sufficient for an internal tool.

## 7. Testing

### 7.1 Layers

| Layer | What it covers |
|---|---|
| `actionlint` on `index-publish.yml` | YAML syntax, GH-expression mistakes, deprecated action versions. Add to `ci.yml` if not already there. |
| `bats` on `build-manifest.sh` | Schema-version detection branches, sha256 + size computation, manifest field correctness. ~5 tests. |
| Manual `workflow_dispatch` dry-run | First invocation runs with `DRY_RUN=1` env, skipping the final `gh release upload`. Logs and step summary still emit. |

### 7.2 Bats coverage for `build-manifest.sh`

Tests under `tools/index-publish/tests/build-manifest.bats`:

1. **PRAGMA user_version returns N → manifest carries N.**
   Fixture: create a tiny SQLite DB with `PRAGMA user_version=4`; run
   the script; assert manifest's `schema_version` is `4`.
2. **PRAGMA empty, `schema_meta` table holds N → manifest carries N.**
   Fixture: create a DB with `PRAGMA user_version=0` and a
   `schema_meta(version int)` table containing `4`; assert manifest's
   `schema_version` is `4`.
3. **Both empty → manifest carries `0` (sentinel).**
   Fixture: DB with `PRAGMA user_version=0` and no `schema_meta`
   table; assert `schema_version` is `0` and `compat_policy` is
   `"primary:crabcc_version;secondary:schema_version"`.
4. **Computed `db_sha256` matches `sha256sum` of the fixture.**
   Assert the manifest's `db_sha256` equals an independently-computed
   sha256 of the raw DB.
5. **Computed `db_zst_size_bytes` < `db_size_bytes` for a real index.**
   Sanity check that zstd actually compressed something.

Mock `crabcc --version` via a thin `crabcc` shim on PATH that echoes
`"crabcc 4.0.0"`.

### 7.3 Dry-run gate

Add a `workflow_dispatch` input `dry_run: { type: boolean, default: false }`.
When `true`:

- Steps 1–7 (build, index, manifest, compress, size guard) run normally.
- Step 8 (release tag selection) runs normally.
- Step 9 (`gh release create` / `gh release upload`) is skipped.
- Step 10 (step summary) writes "DRY-RUN: would publish to <tag>".

Run dry-run from the Actions UI before flipping to live triggers.
Document this in `tools/index-publish/README.md`.

## 8. Out of scope (deferred to follow-up plans)

- **`crabcc index --fetch` consumer.** Own spec. Will read the
  manifest, verify hashes, fall back to local rebuild on mismatch.
- **Reusable workflow** (`on: workflow_call`) callable from other
  peterlodri-sec repos. Refactor after one release cycle of dogfooding.
- **Multi-target indexes** (per-OS, per-arch). The index is SQLite —
  platform-independent at the storage level. Skip until we have a
  proven need.
- **Index GC.** Old rolling-release assets accumulate. Manual deletion
  if it matters; automated GC is YAGNI for an internal tool.
- **Differential / incremental publishing** (publish only diff vs.
  previous). Real fetch + diff cost dominates the consumer side; not
  worth solving on the publisher side first.
- **Signed manifests.** GH release assets are already gated by repo
  push access. Cosign/sigstore later if we move to a public-trust
  consumer model.

## 9. Implementation order

1. `tools/index-publish/build-manifest.sh` + 5 bats tests under
   `tools/index-publish/tests/`. Verify with hand-crafted SQLite fixtures.
2. `.github/workflows/index-publish.yml` with all three triggers (push
   main, push v*, workflow_dispatch). Wire `actionlint` into `ci.yml`
   if not already present.
3. `tools/index-publish/README.md` documenting the deploy and the
   dry-run procedure. Workflow badge into the main `README.md`.
4. Manual `workflow_dispatch` dry-run from the Actions UI. Verify
   manifest looks sane in logs and step summary. Adjust any field
   names if real data exposes naming issues.
5. Flip dry-run off, observe first real run on the next main merge.

## 10. Open questions deferred to implementation

- Does `store.rs` currently set `PRAGMA user_version` or maintain a
  `schema_meta` table? Resolved during T1 (build a real index, probe
  both sources). If neither: `schema_version: 0` permanently, and the
  manifest's `compat_policy` field makes that explicit.
- Exact name of the release tag in `gh release create`: `v4.0.0-index`
  or `index-v4.0.0`? Picking `v<version>-index` and `v<version>-index-latest`
  to sort cleanly next to the binary release tags in the GH UI. Open
  to change during implementation if the GH UI orders them poorly.
- Do we need a `db_format_id` (e.g. "crabcc-sqlite-v4") in the manifest
  to distinguish from a hypothetical future Parquet or RocksDB backend?
  Punt until we have a second format candidate.
