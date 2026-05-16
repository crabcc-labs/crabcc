# SWE Agent Versioning + SQLite Queue

**Date:** 2026-05-16
**Status:** approved
**Scope:** `agents/`, `tools/orchestrator/`

## Problem

During fast iteration on `swe-build` / `swe-fast`, there is no way to tell which agent version produced a given PR comment, GHA step, or DB row. Model, prompt, and tool config are hardcoded in shell scripts with no audit trail.

## Solution

Two independent components that compose at dispatch time:

1. **Versioned agent manifests** — TOML files in `agents/` that define every knob of an agent. The file's git SHA is the version. Changing the file is the version bump.
2. **SQLite-backed task queue** — durable, WAL-mode queue in `_agents.db`. Each row carries `manifest_sha`, so the version that produced every result is queryable forever.

---

## Component 1: Agent Manifests

### Location

```
agents/
  swe-build.toml        # deep, careful — architecture + correctness tasks
  swe-fast.toml         # quick, cheap — lint fixes, small patches
  prompts/
    swe-build.md        # system prompt for swe-build
    swe-fast.md         # system prompt for swe-fast
  README.md             # how versioning works
```

### Manifest format

```toml
[agent]
name        = "swe-build"
description = "Full-depth SWE agent for build, architecture, and correctness tasks"
model       = "openrouter/deepseek/deepseek-v4-pro"

[agent.prompt]
file = "agents/prompts/swe-build.md"

[agent.tools]
allowlist = ["Read", "Write", "Edit", "Bash", "Grep", "Glob"]

[agent.limits]
timeout_minutes = 30
max_retries     = 2
```

### Version derivation

The version of a manifest is `git rev-parse HEAD:agents/<name>.toml` at dispatch time — no manual field to forget to bump. Changing any line in the file or its prompt produces a new SHA automatically.

### Stamp format

Every agent output prepends:

```
<!-- agent: swe-build | sha: abc1234 | model: deepseek-v4-pro | 2026-05-16T20:45Z -->
```

Stamp surfaces:
- PR comment (first line)
- GHA step summary (`>> $GITHUB_STEP_SUMMARY`)
- `agent_tasks` queue row (`manifest_sha` column)

### Resolver script

`tools/orchestrator/resolve-manifest.sh <agent-name>` reads the TOML, validates required fields, prints `MODEL`, `PROMPT_FILE`, `ALLOWLIST`, `MANIFEST_SHA` as env-safe key=value lines. `run-task.sh` sources this instead of hardcoding.

---

## Component 2: SQLite Task Queue

### Database

`~/.crabcc/_agents.db` — separate from `_internal.db` to avoid locking contention. WAL mode enabled on first open.

### Schema

```sql
CREATE TABLE agent_tasks (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    agent         TEXT    NOT NULL,              -- 'swe-build', 'swe-fast'
    manifest_sha  TEXT    NOT NULL,              -- git SHA of agents/<agent>.toml at enqueue time
    payload       TEXT    NOT NULL,              -- JSON: task description + context
    status        TEXT    NOT NULL DEFAULT 'pending',
    -- status: pending | claimed | done | failed | requeued
    created_at    INTEGER NOT NULL DEFAULT (unixepoch()),
    claimed_at    INTEGER,
    completed_at  INTEGER,
    worker_pid    INTEGER,                       -- PID of claiming agent process
    result        TEXT,                          -- JSON output
    error         TEXT                           -- error message on failure
);

CREATE INDEX agent_tasks_status ON agent_tasks(status, created_at);
```

### Queue CLI (`tools/orchestrator/queue.sh`)

```
queue.sh enqueue  <agent> <payload-json>   # insert pending row, print task id
queue.sh claim    <agent>                  # claim oldest pending row for this agent, print id+payload
queue.sh done     <task-id> <result-json>  # mark done
queue.sh fail     <task-id> <error>        # mark failed
queue.sh requeue  <task-id>               # reset to pending (rollback)
queue.sh status   [task-id]               # show row(s)
queue.sh list     [--agent X] [--status Y] [--limit N]
```

All writes use `BEGIN IMMEDIATE` to prevent lost-update under concurrent agents.

### Rollback

"Rollback" is `queue.sh requeue <task-id>` — resets status to `pending`, clears `claimed_at` / `result` / `error`. The worktree commit produced by the failed/unwanted run is reverted separately via `git reset`. The queue row retains the original `manifest_sha`, so replaying with a newer manifest requires a new `enqueue` call (preserving history).

### Integration with orchestrator

`run-task.sh` gains a `--from-queue` flag:
1. Calls `queue.sh claim <agent>`
2. Sources `resolve-manifest.sh` to get model + prompt
3. Dispatches opencode with those params
4. On success: `queue.sh done <id> <result>`
5. On failure: `queue.sh fail <id> <error>`

Concurrency is controlled by the existing semaphore in `dispatch-rotated.sh` — no new mechanism needed.

---

## What is NOT in scope (v1)

- Kafka / Redis / NATS (replaced by SQLite queue)
- Persistent background worker daemon (queue is pull-based, driven by orchestrator dispatch)
- Agent output storage beyond the `result` column (GHA artifacts cover this)
- UI / dashboard

---

## v0.1: LangSmith Datasets + Experiments integration

**Motivation.** A versioned manifest is only as useful as the regression suite you can run it against. LangSmith Datasets give us a curated input set; Experiments give us per-version comparison across the same inputs. The queue we just built is the natural runner.

**References.**
- Org workspace: <https://eu.smith.langchain.com/o/1421ca3c-df50-4e04-8af9-3f3116a2ba4a/datasets?view=new>
- API surface: `POST /datasets/upload-experiment` (LangSmith docs, "Datasets + Experiments" topic)

**Integration points.**

1. **Dataset row → queue task.** A new helper `tools/orchestrator/import-dataset.sh <dataset-name> <agent>` pulls rows via the LangSmith SDK and `queue.sh enqueue`s one task per row. The dataset row's `id` is stored in `payload.langsmith_example_id` so we can correlate later.

2. **Queue run → experiment row.** After a wave finishes (all tasks reach terminal status), `tools/orchestrator/upload-experiment.sh <wave-id>` collects every `agent_tasks` row tagged with that wave, builds the `/datasets/upload-experiment` POST body, and uploads. The experiment name is `<agent>@<manifest_sha_short>` (e.g. `swe-build@abc1234`) so version comparison in the LangSmith UI is the default view.

3. **Validator → evaluation_scores.** The `check-and-done` validator step (proposed in the LangSmith feedback loop above) emits per-row scores: `empty_result` (0/1), `schema_shaped` (0/1), `latency_floor_violation` (0/1), `validator_pass` (0/1 aggregate). These flow into LangSmith as `evaluation_scores`, surfacing silent-failure patterns the way the agentfield-v2 audit identified.

4. **Manifest SHA as experiment metadata.** Every uploaded experiment carries `metadata.manifest_sha` and `metadata.agent_name` — these become filter facets in the LangSmith UI, enabling queries like "show me every `swe-build` experiment from the last 7 days where `validator_pass` ratio dropped below 0.8."

**New files.**

| File | Purpose |
|---|---|
| `tools/orchestrator/import-dataset.sh` | LangSmith dataset → `queue.sh enqueue` fan-out |
| `tools/orchestrator/upload-experiment.sh` | wave results → LangSmith POST `/datasets/upload-experiment` |
| `tools/orchestrator/check-and-done.sh` | validator step between worker `done` and queue persist (closes the silent-failure gap) |

**Schema additions to `agent_tasks` for v0.1.**

```sql
ALTER TABLE agent_tasks ADD COLUMN wave_id TEXT;                 -- groups tasks for a single experiment upload
ALTER TABLE agent_tasks ADD COLUMN langsmith_example_id TEXT;    -- correlation to dataset row
ALTER TABLE agent_tasks ADD COLUMN validator_scores TEXT;        -- JSON {key: score} written by check-and-done
```

Migrations stay additive (per repo convention — see `CLAUDE.md` → "When changing schema").

**Out of scope for v0.1.**

- Custom LangSmith evaluators (LLM-as-judge) — start with deterministic validator scores only
- Auto-promotion / gating on experiment delta — humans review LangSmith UI for now
- Backfill of historical queue rows into datasets — only new runs upload

---

## File checklist

| File | Owner |
|---|---|
| `agents/swe-build.toml` | manifest subagent |
| `agents/swe-fast.toml` | manifest subagent |
| `agents/prompts/swe-build.md` | manifest subagent |
| `agents/prompts/swe-fast.md` | manifest subagent |
| `agents/README.md` | manifest subagent |
| `tools/orchestrator/resolve-manifest.sh` | manifest subagent |
| `tools/orchestrator/queue.sh` | queue subagent |
| `tools/orchestrator/migrate-queue.sh` | queue subagent |
| `tools/orchestrator/tests/queue-smoke.sh` | queue subagent |
