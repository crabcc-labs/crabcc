# Internal Agent — crabcc-viz specialist

You own `crates/crabcc-viz/`. Read `internal_agents/shared.agent.md`
first. This file is the crate-specific context.

## What this crate does

`crabcc-viz` is the **`crabcc serve` localhost dashboard** (issue #64
+ ongoing /live work in #17 / #90).

- **HTTP server** — `tiny_http` based. Sync, threaded, single-user;
  `tokio` would be over-engineering here.
- **Bundled web frontend** — `web/dist/live.html` (React + TS). Built
  out-of-band; the Rust crate just serves the static file. The source
  lives in `web/` (TypeScript + Vite). Don't hand-edit `web/dist/*`.
- **/api/state**, **/api/graph**, **/api/agent-runs**,
  **/api/telemetry**, **/api/agent-kills** — JSON snapshot endpoints
  read from `~/.crabcc/_internal.db` and the per-repo `.crabcc/`.

## Conventions specific to this crate

- **Default bind is 127.0.0.1.** The server is unauthenticated.
  Non-loopback binds emit a stderr warning. Don't change this without
  a real auth story (high blast radius — the dashboard exposes
  architecture).
- **No async runtime.** `tiny_http` is sync. If you find yourself
  reaching for `tokio`, stop and check the perf brief in issue #112.
- **The frontend is a separate build.** `task viz-web-build` runs
  the TS toolchain. The Rust crate's tests use the bundled dist file
  via `include_str!`/`include_bytes!`.
- **Routes are versioned by query-shape, not URL.** Adding a new
  field to `/api/state` is fine; renaming an existing one breaks
  the React frontend's expectation.

## Cross-crate boundaries

- Consumes `crabcc-core::graph` for call-graph data, plus reads from
  the singleton `~/.crabcc/_internal.db` (agent runs / kills /
  cli_calls) and the per-repo `.crabcc/index.db`.
- Consumed only by `crabcc-cli`'s `serve` subcommand.

## Frontend hand-off

When a server-side change needs a frontend update, post a clear
"frontend follow-up" note in the PR description. The frontend agent
flow isn't formalized yet (TS isn't part of the per-crate agent
fan-out). For now, hand off to a human reviewer.
