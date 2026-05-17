# crabcc-cron WL-3 — security research workload

Date: 2026-05-17
Status: Design approved, ready for implementation plan
Predecessor: `2026-05-17-crabcc-cron-shared-and-oss-fix-design.md`
(shared runner + WL-2, merged via PR #565)

## 1. Motivation

WL-2 ships OSS-fix attempts to upstreams. WL-3 turns the same cron
runner toward our own repos and produces an actionable security feed:
every night, walk every Rust repo we own, run `cargo audit`, and for
each advisory emit one finding to the `cron-findings` Chroma collection
with the advisory metadata, the transitive dependency chain, and the
count of files in the repo that touch the vulnerable crate (via
`crabcc fuzzy`). The crabcc usage-count step is the value-add over a
plain bash wrapper around `cargo audit` — it answers "how much of my
code touches this CVE?" in one query.

## 2. Non-goals

- No symbol-level reachability analysis. We don't try to determine
  whether the specific vulnerable function in the crate is on a path
  reachable from our code. Advisory metadata rarely lists affected
  symbols, and full reachability is out of scope for an MVP cron tick.
- No automatic remediation. The workload emits findings; humans (or a
  future workload) decide what to upgrade. No `cargo update` is run.
- No GitHub issue creation. Findings go to Chroma; the eventual morning
  digest workload aggregates and triages.
- No polyglot scanning. Rust-only via `cargo audit`. Adding npm/pip/etc.
  is a future workload, not a WL-3 extension.
- No long-lived state files for dedup. Chroma's content-hash idempotency
  handles it (same `(repo, advisory)` → same `id` → upsert).

## 3. Scope

### 3.1 Audit target

Auto-discovered nightly via `gh repo list peterlodri-sec --no-archived
--limit 200 --json name,defaultBranch`. Each repo is kept iff:
1. Not in `/etc/crabcc-cron/security.toml`'s `[security_deny] exclude` list.
2. Has a `Cargo.toml` at the root (checked via
   `gh api /repos/peterlodri-sec/<name>/contents/Cargo.toml`
   returning 200).

### 3.2 Deployment target

Hetzner box, `/opt/crabcc-cron/`, same as WL-2. Shared layer is reused
verbatim — `crabcc-cron-emit`, `lib/log.sh`, and the Chroma
`cron-findings` collection.

### 3.3 Cadence

Daily, 02:00 UTC. CVE publication rate doesn't justify hourly or
sub-daily checks; daily aligns with the morning-digest workload that
will read these findings.

## 4. Architecture

### 4.1 New files

```
tools/crabcc-cron/
├── jobs/
│   └── security.sh                  # daily entrypoint
├── lib/
│   ├── audit_repos.sh               # repo enumeration helpers
│   └── audit_advisory.sh            # per-advisory finding assembly
├── deploy/
│   ├── security.toml.example        # deny list config
│   └── crabcc-cron.cron             # add daily 02:00 UTC entry (modify)
└── tests/
    ├── unit/
    │   ├── audit_repos.bats         # repo-enumeration unit tests
    │   └── audit_advisory.bats      # advisory-mapping unit tests
    └── e2e/
        └── security_smoke.sh        # full-path smoke with mocked cargo+gh+crabcc
```

### 4.2 Daily flow (`jobs/security.sh`)

1. Source the shared lib (`log.sh`) and the new libs (`audit_repos.sh`,
   `audit_advisory.sh`).
2. Read deny list: `eval "$(crabcc-cron-config-shim
   /etc/crabcc-cron/security.toml)"` — the existing config shim is
   extended in this task to read a `[security_deny] exclude = [...]`
   stanza in addition to the WL-2 stanzas.
3. Enumerate target repos via `enumerate_audit_repos`.
4. For each kept repo, in serial:
   - `git -C /srv/cron-agents/security/<repo> pull` if the dir exists,
     else `gh repo clone peterlodri-sec/<repo> /srv/cron-agents/security/<repo>`.
   - `cd` into the working copy.
   - `crabcc index --refresh 2>/dev/null` — refresh the symbol index;
     ignore failure (advisory finding will be emitted with
     `usage_site_count: null`).
   - `cargo audit --json` → capture stdout to a tempfile, ignore the
     exit code (non-zero is normal when advisories exist).
   - Parse `vulnerabilities.list[]`. For each:
     - Compute `dep_chain` via `cargo tree --invert -p <crate>`.
     - Compute `usage_site_count` via `crabcc fuzzy <crate>` line count.
     - Call `advisory_to_finding` → emit JSONL to stdout.
   - If no advisories: emit one `info` finding `"security scan clean"`
     with direct/transitive dep counts from `cargo tree` line counts.
5. After all repos: emit one `info` summary finding `"security tick
   complete"` with totals and duration.

The entry point is a thin shell wrapper; all reusable logic lives in
the two new `lib/` files for testability.

### 4.3 Index policy

Indexes are built and refreshed on the Hetzner box for this workload.
The follow-up plan **`crabcc-index-as-release-artifact`** (deferred —
see §8) will swap `crabcc index --refresh` for `crabcc index --fetch`
once `.crabcc/index.db` is distributed as a GitHub release asset on
each repo's merge-to-main or version-bump. The shape of WL-3 doesn't
change when that lands; one line of `security.sh` flips.

## 5. Finding shape

### 5.1 Per-advisory finding

```jsonc
{
  "kind": "finding",
  "workload": "security",
  "repo": "peterlodri-sec/crabcc",
  "severity": "error",
  "title": "RUSTSEC-2024-0388: hashbrown <0.15 use-after-free",
  "body": "<advisory headline>\n\n<advisory description>\n\nVulnerable: hashbrown 0.14.5\nFixed in:  hashbrown >= 0.15.0\n\nReverse-dep chain (from cargo tree --invert):\n  hashbrown 0.14.5\n  └── indexmap 2.2.6\n      └── crabcc-core 4.0.0\n          └── crabcc-cli 4.0.0\n\nUsage sites in this repo: 12 files (via crabcc fuzzy hashbrown)\n\nUpgrade: cargo update -p hashbrown",
  "metadata": {
    "advisory_id": "RUSTSEC-2024-0388",
    "crate": "hashbrown",
    "vulnerable_version": "0.14.5",
    "fixed_version": ">=0.15.0",
    "dep_chain_length": 4,
    "usage_site_count": 12,
    "cargo_audit_severity": "high",
    "cwe": "CWE-416",
    "audited_at": "2026-05-17T02:00:00Z"
  }
}
```

### 5.2 Severity mapping

| `cargo audit` `severity` | crabcc-cron `severity` |
|---|---|
| `critical`, `high` | `error` |
| `medium` | `warn` |
| `low`, `informational`, missing | `info` |

The mapping is implemented as a single bash function `map_severity` in
`lib/audit_advisory.sh`.

### 5.3 Clean-scan finding

```jsonc
{
  "kind": "finding",
  "workload": "security",
  "repo": "peterlodri-sec/crabcc",
  "severity": "info",
  "title": "security scan clean",
  "body": "cargo audit found no advisories. <N> direct deps, <M> transitive.",
  "metadata": {"direct_deps": 47, "transitive_deps": 312, "audited_at": "..."}
}
```

Emitted once per scanned repo when `vulnerabilities.list[]` is empty.
Useful for tracking "repo X has been clean for N consecutive nights"
later.

### 5.4 Tick-summary finding

```jsonc
{
  "kind": "finding",
  "workload": "security",
  "repo": "",
  "severity": "info",
  "title": "security tick complete",
  "body": "Scanned <N> repos, <M> advisories, <K> repos clean. Duration: <S>s.",
  "metadata": {"repos_scanned": 8, "advisories_total": 3, "repos_clean": 7, "duration_s": 142}
}
```

Always emitted, exactly once per tick.

### 5.5 Idempotency

`id` is auto-computed by `crabcc-cron-emit` as
`sha256(workload:repo:title)`. Title includes the advisory ID, so the
same `(repo, advisory)` pair on consecutive nights produces the same
id — Chroma upserts. The `body` and `metadata` (especially
`usage_site_count` and `audited_at`) refresh each night.

## 6. Failure handling

No state, no cooldown, no per-repo dedup gates. Every advisory is
re-scanned every night. Each per-repo failure is isolated and emits a
diagnostic finding before continuing to the next repo.

| Failure | Behavior |
|---|---|
| `gh repo clone` / `git pull` fails | Emit `error` finding `repo: clone failed`. Continue. |
| `Cargo.toml` exists but no `Cargo.lock` | Emit `info` finding `repo: skipped (no Cargo.lock)`. Continue. |
| `cargo audit` produces malformed JSON | Emit `error` finding `repo: cargo-audit failed: <stderr tail truncated to 200 chars>`. Continue. |
| `cargo audit` exits non-zero with valid JSON (its default when advisories present) | **Normal path** — parse advisories and emit each. Non-zero exit is not a failure. |
| `crabcc index` fails or `crabcc fuzzy` returns garbage | Emit advisory finding anyway with `metadata.usage_site_count = null` + `metadata.usage_count_unavailable_reason = "<short reason>"`. Don't block. |
| `cargo tree --invert` fails | Emit advisory finding with `metadata.dep_chain_length = null` and the chain section in the body replaced by `"<dep-chain unavailable>"`. Don't block. |
| Whole repo iteration aborts unexpectedly | Emit `error` finding `repo: iteration aborted: <reason>`. Continue. |

**Disk hygiene:** `/srv/cron-agents/security/<repo>/` is reused across
runs. `git pull` keeps it current. If pull fails (force-pushed branch,
local divergence, etc.), the wrapper `rm -rf`s the dir and re-clones
once. No 24h cleanup — these are long-lived working trees.

**Concurrency:** all repos processed serially in a single bash for-loop.
For 5–20 typical Rust repos, serial finishes well under the 1-hour
budget. Parallelization is a future optimization, not an MVP need.

## 7. Cron entry

Appended to `tools/crabcc-cron/deploy/crabcc-cron.cron`:

```cron
# WL-3 security audit — daily 02:00 UTC
0 2 * * * deploy  /opt/crabcc-cron/jobs/security.sh 2>&1 \
  | tee >(systemd-cat -t crabcc-cron-security) \
  | /opt/crabcc-cron/bin/crabcc-cron-emit
```

Same pipe shape as WL-2. Findings stream to Chroma; logs stream to
`journalctl -t crabcc-cron-security`.

## 8. Out of scope (deferred to follow-up plans)

- **`crabcc-index-as-release-artifact`** — publish `.crabcc/index.db`
  as a versioned GitHub release asset on merge-to-main or version-bump.
  Lets WL-1, WL-3, and the morning digest skip rebuilding indexes.
  Spec it separately; WL-3 swaps `crabcc index --refresh` for
  `crabcc index --fetch <repo>` in one line when it lands.
- **GitHub issue creation for HIGH/CRITICAL advisories.** Useful but
  needs dedup state; the morning-digest workload is the better home.
- **Polyglot scanning** (`osv-scanner`, `npm audit`, `pip-audit`,
  Snyk, Trivy). Separate spec.
- **Symbol-level reachability** (`crabcc graph walk <fn> --dir
  callers`) when advisories list affected functions. Most advisories
  don't, so the precision gain is marginal until upstream advisory
  metadata improves. Revisit annually.
- **Parallel repo scans.** Serial is fine for current repo counts.
  Optimize if a tick exceeds the 1-hour cron budget.
- **Suppression workflow** (mark "I know about this, suppress for 30
  days"). Future workload concern, not WL-3's.

## 9. Testing

Match the WL-2 pattern: bats unit tests for the pure pieces, a single
e2e smoke script for the full path.

| File | Coverage |
|---|---|
| `tests/unit/audit_repos.bats` | `enumerate_audit_repos` against mocked `gh repo list` + mocked `gh api /contents/Cargo.toml`: Rust-only filter, denylist filter, archived filter, empty list returns empty. ~4 tests. |
| `tests/unit/audit_advisory.bats` | `advisory_to_finding` over `cargo audit` JSON + `cargo tree` text + `crabcc fuzzy` count: severity mapping for all 4 input levels, title format, dep chain string assembly, `usage_site_count = null` fallback, fixed-version extraction, missing `cwe` field tolerated. ~6 tests. |
| `tests/e2e/security_smoke.sh` | End-to-end with mocked `gh`, mocked `cargo audit` (canned JSON with 1 advisory), mocked `cargo tree`, mocked `crabcc`. Assert: correct finding count (advisories + clean-scan per repo + 1 summary), correct severity per advisory, exit 0. |

Mock strategy reuses the fake-binary-on-PATH pattern from WL-2's
`emit_chroma_post.bats` and `oss_fix_dryrun.sh`. Each fake routes by
first two argv tokens.

After WL-3 lands: 48 (WL-2) + ~10 (WL-3 unit) = ~58 unit tests, plus 2
e2e smoke scripts (`oss_fix_dryrun.sh`, `security_smoke.sh`).

## 10. Implementation order

1. Extend `crabcc-cron-config-shim` to emit `SECURITY_DENY` array from
   the new `[security_deny] exclude` stanza. Add a bats test for it.
2. `lib/audit_repos.sh`: `enumerate_audit_repos` function + unit tests.
3. `lib/audit_advisory.sh`: `map_severity`, `advisory_to_finding`, plus
   helpers for dep chain + usage count assembly. Unit tests.
4. `jobs/security.sh`: the wrapper that wires everything together.
5. `deploy/security.toml.example` + `deploy/crabcc-cron.cron` cron line.
6. `tests/e2e/security_smoke.sh`: full-path smoke with mocked binaries.
7. README addition documenting the new workload.

Each step ends with a TDD commit pushed to `origin`.

## 11. Open questions deferred to implementation

- Exact bash representation of the `cargo audit` JSON parse. Likely
  one `jq -c '.vulnerabilities.list[]'` loop. Tests pin the shape.
- Where to source `cargo audit` itself on Hetzner: install via
  `cargo install cargo-audit` in `deploy/install.sh`? Add a preflight
  check that fails the install with a clear message if it's missing?
  Decide while implementing step 5.
- Whether the deny list should also cover individual advisory IDs (not
  just repos). Punt to a follow-up — none of our own repos has
  long-standing accepted-risk advisories yet.
