# crabcc-cron — shared runner + OSS-fix dispatcher (WL-2)

Date: 2026-05-17
Status: Design approved, ready for implementation plan
Author: brainstormed in session, condescending-chaplygin-f98e11
Scope: this spec covers two sub-projects (shared layer + WL-2). Sibling
workloads (WL-1 cross-repo drift, WL-3 security research, morning digest)
get their own specs.

## 1. Motivation

We have idle compute (Hetzner box, opencode + deepseek-v4-pro budget, the
Claude Code harness) and recurring opportunities to convert that compute
into value: upstream OSS PRs, security findings, cross-repo regressions.
The first cron agent productizes one slice of that: a periodic dispatcher
that picks one tractable open-source issue, throws an agent at it in an
isolated clone, and opens a draft PR if the fix lands cleanly. All
findings flow into a shared Chroma collection so later agents and Claude
sessions can query them as RAG context.

The design decomposes into three sub-systems, of which this spec covers
the first two:

1. **Shared runner + Chroma sink** (this spec, Section A) — workload
   contract, sink, deployment.
2. **WL-2 OSS-fix dispatcher** (this spec, Section B) — the first
   workload built on the shared layer.
3. WL-1 drift, WL-3 security, morning digest — future specs.

## 2. Non-goals

- No replacement for human review. PRs are always opened as draft and the
  user merges them.
- No multi-agent coordination. Each workload tick is independent.
- No autonomous learning. Templates and config are static, edited in git.
- No new long-running daemon (other than cron itself). Workloads are
  short-lived scripts.
- No model on Hetzner. Embeddings are server-side at Chroma Cloud.

## 3. Section A — Shared layer

### 3.1 Deployment target

Linux (Hetzner box). User `deploy`. Everything under `/opt/crabcc-cron/`
and `/etc/crabcc-cron/`.

### 3.2 Filesystem layout

```
/opt/crabcc-cron/
├── bin/
│   ├── crabcc-cron-emit         # stdin JSONL → Chroma POST
│   └── crabcc-cron-spool-flush  # retry queue drainer (cron'd)
├── jobs/
│   ├── oss-fix.sh               # WL-2 entrypoint
│   ├── drift.sh                 # WL-1 placeholder (future)
│   └── security.sh              # WL-3 placeholder (future)
├── templates/
│   └── oss-fix.md               # agent prompt template
├── spool/                       # NDJSON of findings that failed to ship
└── state/                       # per-workload state files

/etc/crabcc-cron/env             # chmod 600, sourced by every job entry:
                                 #   GH_TOKEN
                                 #   ANTHROPIC_API_KEY
                                 #   OPENCODE_API_KEY (or equivalent)
                                 #   CHROMA_HOST
                                 #   CHROMA_TENANT
                                 #   CHROMA_DATABASE
                                 #   CHROMA_API_KEY
                                 #   OPENCODE_MODEL=deepseek-v4-pro

/etc/cron.d/crabcc-cron          # crontab entries (see 3.6)
```

### 3.3 Workload contract

Every `jobs/*.sh` is a standalone bash script. Its stdout is JSON Lines
discriminated by a `kind` field:

```json
{"kind": "log",     "level": "info|warn|error", "msg": "..."}
{"kind": "finding", "id": "<sha256>", "workload": "oss-fix",
 "repo": "rust-lang/cargo", "severity": "info|warn|error",
 "title": "short headline",
 "body": "longer description (what gets embedded)",
 "metadata": { ... arbitrary scalars ... }}
```

All stdout (both `log` and `finding` lines) is forwarded to journalctl
via `systemd-cat -t crabcc-cron-<wl>` so
`journalctl -t crabcc-cron-oss-fix` works without running the workloads
as systemd units. Findings appearing in journalctl is intentional: it
keeps the same human-readable narrative available for debugging without
having to query Chroma.

The same stream is also piped to `crabcc-cron-emit`, which filters for
`kind == "finding"` (ignoring `log` lines) and then:
1. Validates required fields (`id`, `workload`, `severity`, `title`,
   `body`).
2. Computes the `id` if missing as
   `sha256(workload || ":" || repo || ":" || canonical_event_key)`.
3. POSTs upsert to the Chroma collection `cron-findings` with:
   - document: `body`
   - id: `id`
   - metadata: `{workload, repo, severity, title, ts, ...metadata}` —
     the workload-supplied `metadata` dict is flattened with a `meta.`
     prefix to avoid collision with reserved keys.
4. On any network/HTTP error: appends the raw finding line to
   `/opt/crabcc-cron/spool/<YYYY-MM-DD>.ndjson` and exits 0 (the job
   shouldn't see emit failures).

Stdin format is one JSON object per line. Empty lines and `kind` values
other than `log` and `finding` are ignored with a warning to stderr.

### 3.4 Chroma sink

- Collection name: `cron-findings`. Auto-created with cosine distance
  and Chroma Cloud's default embedder on first emit.
- Embeddings: server-side. No model on Hetzner.
- Reruns are idempotent because the `id` is content-hashed. Same finding
  → Chroma upsert is a no-op.
- Body length cap: 8 KiB. Longer bodies are truncated with a
  `[truncated]` marker; full body is preserved in `metadata.body_full`
  iff it fits in the 16 KiB metadata limit.

### 3.5 Spool drainer

`crabcc-cron-spool-flush`:
1. Reads every `spool/*.ndjson` (oldest first).
2. For each line, re-POSTs to Chroma.
3. On success: removes the line (rewrites the file).
4. On failure: leaves the line; aborts the file; tries the next.
5. After processing, deletes empty spool files.

Runs every 15 minutes via its own cron entry. Idempotent.

### 3.6 Cron configuration

`/etc/cron.d/crabcc-cron`:

```cron
SHELL=/bin/bash
BASH_ENV=/etc/crabcc-cron/env
MAILTO=""

# WL-2 OSS-fix dispatcher — every 4h
0 */4 * * * deploy  /opt/crabcc-cron/jobs/oss-fix.sh 2>&1 \
  | tee >(systemd-cat -t crabcc-cron-oss-fix) \
  | /opt/crabcc-cron/bin/crabcc-cron-emit

# Spool drainer — every 15 minutes
*/15 * * * * deploy /opt/crabcc-cron/bin/crabcc-cron-spool-flush \
  2>&1 | systemd-cat -t crabcc-cron-spool
```

Future workloads (WL-1, WL-3) get their own entries following the same
pipe shape.

### 3.7 Secrets handling

`/etc/crabcc-cron/env` is `chmod 600 root:deploy`. Bash's `BASH_ENV`
sources it on shell startup, so every cron-spawned `bash` already has
the variables exported. Workloads don't need to source anything
themselves.

Rotation: the file is the only place tokens live on the box.
`crabcc-cron-emit` doesn't log token values — log/journalctl output is
safe to share.

### 3.8 Observability

- `journalctl -t crabcc-cron-<workload>` — per-workload run history.
- `journalctl -t crabcc-cron-spool` — sink retry history.
- Chroma `cron-findings` collection — durable, queryable record of
  every finding ever emitted.
- No metrics endpoint. If we need one later, a sidecar can scrape
  journalctl.

## 4. Section B — WL-2 OSS-fix dispatcher

### 4.1 Cadence

Every 4 hours (6 ticks per day). Each tick attempts at most one fix.

### 4.2 Upstream curation

Config file `/etc/crabcc-cron/oss-fix.toml`:

```toml
[tier1_my_deps]
# Auto-discovered from `cargo metadata` of every repo under the listed
# roots. Anything you depend on is implicitly in scope.
auto_discover = true
local_repo_roots = ["~/workspace"]

[tier2_curated]
# Hand-picked upstreams worth investing in.
include = [
  "rust-lang/cargo",
  "tokio-rs/tokio",
  "tree-sitter/tree-sitter",
  "rusqlite/rusqlite",
]

[tier3_deny]
# Override: never touch these.
exclude = ["serde-rs/serde"]
```

The dispatcher computes the working set as
`(tier1 ∪ tier2) \ tier3`, refreshed at the start of each tick.

For Hetzner to do `cargo metadata` against the user's repos, either:
(a) the local roots are rsync'd nightly to Hetzner (separate concern,
out of scope here), or (b) `auto_discover` runs against
`gh repo list peterlodri-sec --limit 200` filtered to repos that have a
`Cargo.toml` at the root (cheaper, no rsync needed). **Default: (b).**
Spec assumes the GitHub-API-driven path.

### 4.3 Issue selection

Per upstream, the dispatcher queries:

```
gh issue list --repo <owner/repo>
  --label "good first issue,help wanted,E-easy,D-easy"
  --state open
  --json number,title,labels,assignees,createdAt,updatedAt,comments
```

An issue is **eligible** iff all of:

- `assignees` is empty
- `linkedBranches` is empty (`gh issue view <n> --json linkedBranches`)
- `7d <= now - createdAt <= 180d`
- `now - updatedAt > 30d` (not actively discussed)
- `comments.totalCount <= 10`
- `state/<owner>--<repo>--<issue>.attempted` file does not exist

Across all upstreams, the dispatcher picks the **lowest issue number on
the upstream with the most stars** to bias toward visible, high-value
contributions. Ties broken by upstream name (alphabetical).

If no eligible issue is found, the dispatcher emits one finding with
`status: "no_eligible_issue"` and exits 0.

### 4.4 Per-attempt sandbox

```
/srv/cron-agents/oss-fix/<owner>--<repo>--<issue>/
├── clone/              # gh repo clone, fresh, branched as claude-cron/fix-<issue>
├── opencode.log        # captured stdout of `opencode run`
├── prompt.md           # rendered from /opt/crabcc-cron/templates/oss-fix.md
└── status              # one of: running | fixed | tests-failed | no-fix | timeout | error
```

Cleanup: the spool drainer also deletes attempt directories whose
`status` file is in a terminal state and is older than 24h.

### 4.5 Agent invocation

```
opencode run \
  --model "$OPENCODE_MODEL" \
  --cwd "$ATTEMPT_DIR/clone" \
  --prompt-file "$ATTEMPT_DIR/prompt.md" \
  --max-tokens "$OSS_FIX_MAX_TOKENS" \
  --timeout "$OSS_FIX_TIMEOUT" \
  > "$ATTEMPT_DIR/opencode.log" 2>&1
```

Steady-state targets: `OSS_FIX_MAX_TOKENS=200000`, `OSS_FIX_TIMEOUT=30m`.
Initial rollout values are lower (see Section 6, step 4) and live in
`/etc/crabcc-cron/env`.

Prompt template (`/opt/crabcc-cron/templates/oss-fix.md`):

```
You are working on issue #{N} in {repo}: "{title}".

Body:
{issue.body}

Repo root: . (you are already inside the working clone)
Branch:    claude-cron/fix-{N}

Task:
1. Read the issue. If unclear or actually a design discussion → STOP and
   write the literal string "STATUS=no-fix" on its own line followed by
   a one-paragraph reason.
2. Find the failing code/test, OR write a reproducing test if none
   exists.
3. Implement the minimal fix.
4. Run the test command for this repo: {test_cmd}. All must pass.
5. If green → commit. Don't push, don't open a PR (the wrapper does
   that). Final line of your output MUST be "STATUS=fixed".
6. If you can't make tests pass within budget → write "STATUS=tests-failed"
   followed by the diff you tried.
7. If you hit the timeout, the wrapper will mark "STATUS=timeout"
   automatically.

Hard rules:
- Single-file change preferred. Refuse multi-crate refactors.
- No new dependencies.
- Match existing code style; run any formatter the repo configures.
- No telemetry, debug prints, or commented-out code in the final diff.
```

`{test_cmd}` is detected by the wrapper:
- `Cargo.toml` at root → `cargo test --workspace`
- `package.json` at root → `npm test --silent`
- `pyproject.toml` at root → `pytest -q`
- override via `oss-fix.toml` `[repos."<owner>/<repo>"].test_cmd` if
  any of the above is wrong.

### 4.6 Outcome handling

After opencode exits:

| Last line of opencode.log | Wrapper action |
|---|---|
| `STATUS=fixed` | run test_cmd one more time as a sanity gate; if green, `git push origin claude-cron/fix-<n>`, then `gh pr create --draft` with the PR body in 4.7 |
| `STATUS=tests-failed` | no push, no PR; emit `tests_failed` finding |
| `STATUS=no-fix` | no push, no PR; emit `no_fix` finding |
| (none, and exit was timeout) | no push, no PR; emit `timeout` finding |
| (none, non-zero exit) | emit `error` finding |

In all cases, write the terminal value to `status` and mark
`state/<owner>--<repo>--<issue>.attempted` (touch is sufficient — content
not used).

### 4.7 PR identity and body

Identity: PRs open from the user's personal GitHub account, using the
`GH_TOKEN` in `/etc/crabcc-cron/env`. Always opened as **draft**.

Title: `[draft] fix: <first line of agent's commit message>`

Body:

```
Closes #{N}

<agent-supplied summary of the fix, copied from commit body if present>

---
This PR was drafted by an automated agent (opencode + {OPENCODE_MODEL})
running on cron. I'll review and finalize before requesting merge.

Run log: <gist URL of opencode.log>
```

The run log is uploaded as a private gist via `gh gist create`, link
captured before PR creation.

### 4.8 Rate limits

Three caps, all checked at the start of each tick:

| Cap | Source of truth | Behavior at cap |
|---|---|---|
| Global open agent-drafted PRs ≤ 3 | `gh pr list --author @me --search 'in:body "automated agent"'` | emit `at_cap` finding, exit |
| Per-upstream: 1 PR/week | `state/<owner>--<repo>.last_pr` (touched on success) | skip this upstream when iterating eligibility |
| Per-issue: one-shot ever | `state/<owner>--<repo>--<issue>.attempted` | drop from eligibility |

### 4.9 Findings emitted

Every tick emits exactly one finding, even on no-op. Used `status`
values, with severity and emit conditions:

| `status` | severity | Emitted when |
|---|---|---|
| `pr_opened` | info | draft PR opened upstream |
| `tests_failed` | warn | agent ran, tests didn't pass |
| `no_fix` | info | agent declined (unclear / out of scope) |
| `timeout` | warn | hit time or token budget |
| `at_cap` | info | global PR cap reached, skipped tick |
| `no_eligible_issue` | info | nothing in any upstream passed filters |
| `error` | error | agent crashed, network failure, internal bug |

Metadata always includes:
`{ upstream, issue_number, attempt_dir, opencode_exit_code, duration_s }`

`pr_opened` additionally includes `pr_number`, `pr_url`, `gist_url`,
`branch`.

### 4.10 Failure modes and recovery

- **Hetzner down**: cron doesn't fire. On next boot, no backlog catch-up
  (each tick is independent).
- **Chroma down**: emit spools; `spool-flush` drains when it recovers.
- **gh API rate limited**: `gh` exits non-zero, wrapper emits `error`
  finding and exits 0. Next tick retries naturally.
- **opencode hangs**: 30-minute timeout, then SIGKILL'd by the wrapper.
  Emits `timeout`.
- **Disk fills**: cleanup is automatic after 24h of terminal status,
  but the dispatcher refuses to start if `df --output=avail /srv` shows
  less than 5 GiB free. Emits `error`.

## 5. Testing strategy

- **Unit (Section A)**: `crabcc-cron-emit` has a `--dry-run` mode that
  prints the POST body instead of sending. Fixture-based tests for
  schema validation, id hashing, metadata flattening, body truncation.
- **Integration (Section A)**: spin up a real Chroma collection in a
  container, run a workload through end-to-end, assert findings land.
  Spool drainer test: kill Chroma mid-run, assert spool grows, restart
  Chroma, assert spool drains.
- **Unit (Section B)**: issue-selection logic with `gh` API fixtures
  (mocked). Eligibility predicate has a property-based test.
- **Integration (Section B)**: end-to-end on a sacrificial GitHub repo
  controlled by the user. The repo has a curated "good first issue"
  with a trivial fix and a passing test. Wrapper runs against it, asserts
  draft PR opens. Cleanup deletes the PR + branch after assertion.
- **Manual smoke**: first deployment runs with `OSS_FIX_DRY_RUN=1` env,
  which goes through everything except the final `gh pr create` and
  `git push`. Logs and findings still emit. Run for a week before going
  live.

## 6. Implementation order

1. `crabcc-cron-emit` + Chroma collection bootstrap + spool/drainer
   (Section A only). Smoke test with a hand-written test workload that
   emits 3 known findings.
2. Wire up `/etc/cron.d/crabcc-cron` skeleton with the spool drainer.
3. WL-2 wrapper script (`oss-fix.sh`) without the agent step — just
   issue selection, eligibility filtering, sandbox setup, findings.
   Validate with `OSS_FIX_DRY_RUN=1`.
4. Add the opencode invocation. Initially with `--max-tokens 50000` and
   `--timeout 10m` to limit blast radius. Tune up after first 10
   real runs.
5. Flip `OSS_FIX_DRY_RUN` off. Monitor for a week.

## 7. Open questions deferred to implementation

- Exact `gh repo list` filter for `tier1_my_deps` (which orgs to
  enumerate, how to dedupe forks). Will be settled when writing
  the upstream-curation function — config file format already supports
  the answer regardless.
- Whether to retry `tests_failed` issues after 30 days (currently:
  never). Punt to first review cycle.
- Whether to extend `oss-fix.toml` with per-repo prompt overrides.
  Punt until we have ≥10 real attempts and see what doesn't work
  generically.
