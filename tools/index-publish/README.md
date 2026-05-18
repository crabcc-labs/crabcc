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

```bash
# Lint
task index-publish-lint

# Unit tests for the manifest assembly script
task index-publish-test

# Manual end-to-end run (requires sqlite3 + zstd + a built crabcc):
cargo build --release -p crabcc-cli
./target/release/crabcc index
mkdir -p /tmp/index-out
tools/index-publish/build-manifest.sh \
  --db .crabcc/index.db \
  --crabcc-binary ./target/release/crabcc \
  --source-repo peterlodri-sec/crabcc \
  --source-sha "$(git rev-parse HEAD)" \
  --output-dir /tmp/index-out
ls -la /tmp/index-out/
cat /tmp/index-out/crabcc-index-*.manifest.json | jq .
```

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
