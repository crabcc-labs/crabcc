# tools/orchestrator

Bash helpers for the crabcc agent orchestration pipeline.

## Environment variables

| Variable | Required | Default | Description |
|---|---|---|---|
| `LANGSMITH_API_KEY` | yes (LangSmith helpers) | — | API key sent as `X-API-Key` header |
| `LANGSMITH_ENDPOINT` | no | `https://eu.api.smith.langchain.com` | LangSmith API base URL |
| `LANGSMITH_PROJECT` | no | — | Project name tag (informational; passed through in payloads where used) |
| `LANGSMITH_DATASET_DEFAULT` | no | — | Default dataset name for scripts that accept it optionally |
| `AGENTS_DB` | no | `~/.crabcc/_agents.db` | SQLite database path for the agent task queue |
| `ORCH_RUNTIME` | no | `~/.orchestrator` | Runtime directory for worktrees, locks, and logs |

## Scripts

### `queue.sh`

SQLite-backed agent task queue CLI.

```
queue.sh enqueue  <agent> <payload-json>
queue.sh claim    <agent>
queue.sh done     <task-id> <result-json>
queue.sh fail     <task-id> <error-msg>
queue.sh requeue  <task-id>
queue.sh status   [task-id]
queue.sh list     [--agent X] [--status Y] [--limit N]
```

### `migrate-queue.sh`

Create or upgrade `$AGENTS_DB` with the `agent_tasks` schema. Idempotent.

```bash
migrate-queue.sh
```

### `langsmith.sh`

Thin curl wrapper for the LangSmith API. Requires `LANGSMITH_API_KEY`.

```bash
# Check connectivity and auth.
langsmith.sh ping

# Fetch a dataset record (id, name, example_count).
langsmith.sh get-dataset my-eval-dataset

# List examples for a dataset id.
langsmith.sh list-examples <dataset-id> [--limit 50]

# Upload an experiment result set.
langsmith.sh upload-experiment /path/to/body.json
```

All operations emit structured log lines to stderr:
```
[langsmith] <iso-ts> INFO|WARN|ERROR <event> key=val ...
```

### `import-dataset.sh`

Fetch a LangSmith dataset and enqueue every example as an agent task.
Prints the generated `wave_id` on stdout (pipe to `upload-experiment.sh` later).

```bash
wave_id="$(import-dataset.sh my-eval-dataset my-agent)"
# → import-my-eval-dataset-1716000000
```

Enqueue flags (`--wave-id`, `--example-id`) are passed to `queue.sh` when
supported; if not, the identifiers are embedded in the payload JSON and a
WARN is logged.

### `upload-experiment.sh`

Collect all terminal (`done` or `failed`) queue rows for a wave and upload
them to LangSmith as an experiment. Prints the experiment URL on stdout.

```bash
upload-experiment.sh import-my-eval-dataset-1716000000
# → https://eu.smith.langchain.com/projects/<experiment-id>
```

Queries the queue via `queue.sh list --wave <wave-id>` when supported;
falls back to a direct `sqlite3` query against `$AGENTS_DB`.

### `run-task.sh`

Run one plan task in an isolated git worktree with a coder + reviewer pass.
See the file header for full usage and exit-code documentation.

### `dispatch-rotated.sh`

Wrap `run-task.sh` with per-task model rotation, preflight checks, and
staggered parallel dispatch.

```bash
dispatch-rotated.sh <plan-name> <task-id> [<task-id>...]
dispatch-rotated.sh --preflight <plan-name> <task-id> [<task-id>...]
```

### `integrate-wave.sh`

Cherry-pick all task branches of a wave onto the current branch and clean up.

```bash
integrate-wave.sh <plan-name> <task-id> [<task-id>...]
```

## Design reference

See `docs/superpowers/specs/` for the queue schema design and the LangSmith
logging contract that governs the structured log format used by these helpers.

## Tests

```bash
# Queue end-to-end smoke test (no network required):
tools/orchestrator/tests/queue-smoke.sh

# LangSmith helpers smoke test (skips cleanly when no API key is set):
tools/orchestrator/tests/langsmith-smoke.sh
```
