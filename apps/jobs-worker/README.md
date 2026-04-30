# crabcc jobs-worker

BullMQ worker for the wire format encoded by `crabcc-core::jobs::submit_async`
(issue #109). Bun + TypeScript; one process, one Worker per queue.

## Status

**Scaffold-quality**. The handler is currently a passthrough echo — proves
the wire-protocol round-trip end-to-end without burning real compute.
Real per-queue logic (shell out to `crabcc agent --backend ollama`,
run `crabcc index`, fan out flow children, etc.) lands in follow-up
branches per issue #109's AC.

## Queues

Defined statically in `src/index.ts` to match the JobSpec.queue values
the Rust submitter sends:

- `agent:run` — single agent invocation
- `agent:flow` — parent-child DAG (warp-speed-audit, etc.)
- `repo:index` — long-running `crabcc index` triggered by file events
- `repo:reindex` — scheduled / repeatable reindex

## Run locally

```bash
cd apps/jobs-worker
bun install
REDIS_URL=redis://127.0.0.1:6379 bun run dev
```

Or via the dev compose (auto-starts alongside Redis under the `jobs`
profile):

```bash
docker compose -f install/dev/docker-compose.yml --profile jobs up -d --wait
docker compose -f install/dev/docker-compose.yml logs jobs-worker -f
```

## Observability

All stdout is **JSON-line structured logs** — one event per line, each
with an `event` discriminator:

| event | when |
|---|---|
| `jobs_worker.boot` | worker process started, queues registered |
| `jobs_worker.redis_connected` / `redis_error` | ioredis connection state |
| `jobs_worker.job_start` | a job was popped from `bull:<queue>:wait` |
| `jobs_worker.job_complete` | handler returned successfully |
| `jobs_worker.job_failed` | handler threw or BullMQ marked failed |
| `jobs_worker.shutdown_start` / `shutdown_done` | SIGTERM / SIGINT received |

Each event carries `queue`, `job_id`, `name` where applicable. The
shape is intentionally compatible with the Crabcc.app menubar's
existing JSON-lines telemetry consumer (`installer/Crabcc.app/menubar.swift`).

### macOS app integration

**Planned follow-up.** The Crabcc.app menubar (issue #107 Part A)
already tails `~/Library/Logs/Crabcc/*.log`; pointing the worker's
stdout at a file there gives the menubar a "Recent Jobs" submenu for
free. Today the worker logs to stdout only — the dev compose's
`logging.driver: json-file` keeps them in Docker's log subsystem.

## Submitting jobs

From Rust (with `--features jobs` on `crabcc-core`):

```rust
use crabcc_core::jobs::{submit, JobSpec, Options};

let spec = JobSpec {
    queue: "agent:run".into(),
    name: "warp-speed".into(),
    data: serde_json::json!({ "prompt": "ping" }),
    priority: None, delay_ms: None, attempts: Some(3),
};
let id = submit(&Options::default(), spec)?;
println!("job id: {id}");
```

The id returned is the BullMQ-allocated numeric id; `crabcc-core::jobs::status`
walks the per-queue keys to look up the current state.

## Out of scope (issue #109 follow-ups)

- Real per-queue handlers — today's echo is intentional.
- BullMQ flows (parent → children DAG) — needs the `addBulk` /
  `addFlow` API integration.
- Repeatable jobs — needs the `repeatJobKey` Lua script.
- File-based logging hand-off to `~/Library/Logs/Crabcc/`.
- Per-job tracing → `rotel` OTLP collector (issue #86 dependency).
