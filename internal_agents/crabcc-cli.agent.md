# Internal Agent — crabcc-cli specialist

You own `crates/crabcc-cli/`. Read `internal_agents/shared.agent.md`
first. This file is the crate-specific context.

## What this crate does

`crabcc-cli` is the **top-of-stack** binary crate. It owns:

- **`crabcc` + `ccc` binaries** — clap subcommand surface. `main.rs`
  is the dispatch hub; per-subcommand modules under `src/`.
- **Agent runtime** (`agent.rs`, `agent_runs_db.rs`, `agent_guard.rs`,
  `manager.rs`) — `crabcc agent --run`, the singleton `_internal.db`
  lifecycle store, the 20-min stuck/zombie guard, the heartbeat
  daemon.
- **Telemetry** (`telemetry.rs`) — KPI-only release filter +
  tracing-appender JSON sink (issue #90).
- **MCP server entry** (`crabcc --mcp` shim around the `crabcc-mcp`
  library crate).
- **Status surface** (`status.rs`) — `--status-line` / `--is-repo`.

## Conventions specific to this crate

- **Subcommand tests live under `tests/agent_e2e.rs` etc.** — real
  end-to-end runs against a tempdir, no mocking. The `dry_run` flag
  on `AgentRequest` is the canonical "don't actually launch claude"
  path.
- **The CLI hint banner** (`crabcc sym X` → "ccc find X" stderr) is
  intentional UX — preserve it. Suppression env vars: `CCC_NO_WARN=1`
  (called from ccc combo binary), `CRABCC_NO_HINT=1` (user opt-out).
- **`ManagerGuard` at the top of `main()`** — every CLI invocation
  records into `cli_calls` via begin/Drop. Skipped only for
  `manager daemon` itself (avoids self-referential rows).
- **Argv hygiene** — args containing secrets (API keys) shouldn't
  land verbatim in `cli_calls.args`. Future redactor goes here.

## What ships in this crate's binary

- `crabcc` — full surface
- `ccc` — combo CLI; the `find` subcommand routes to crabcc's
  `sym` / `refs` / `callers` / `fuzzy` / `prefix` / `grep`. See
  `src/bin/ccc.rs`.

## Don't break

- The clap subcommand IDs (renaming a subcommand breaks every user's
  shell history, scripts, hooks, and the slash command files in
  `commands/`).
- `crabcc --version` output format. Several scripts grep it.
- `cli_calls` schema — additive only.

When in doubt, check what `crabcc-cli` exports. Nothing depends on
this crate (it's the top).
