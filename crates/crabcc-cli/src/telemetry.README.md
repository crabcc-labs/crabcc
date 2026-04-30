# crabcc telemetry — module overview

Issue #86 + #90. Source: `crates/crabcc-cli/src/telemetry.rs`

## Architecture

```
crabcc process
  │
  ├─ stderr layer      (always-on, non-blocking, RUST_LOG filter)
  │
  ├─ jsonl layer       ─► .crabcc/telemetry.jsonl  ◄── /api/telemetry (viz)
  │   KPI events only                                   tailed by /live
  │
  ├─ OTLP layer        ─► OTEL_EXPORTER_OTLP_ENDPOINT/v1/traces
  │   (opt-in)              │
  │                        rotel :4318  ─► in-memory ring ─► /live panel
  │                                                          (issue #86)
  │
  └─ Telegram layer    ─► WARN/ERROR + KPI → Telegram chat
      (opt-in)
```

## Storage contract

| Store | What it holds | Telemetry data? |
|-------|---------------|-----------------|
| `.crabcc/index.db` | Symbol index | **Never** |
| `~/.crabcc/_internal.db` | Agent run lifecycle | **Never** |
| `.crabcc/telemetry.jsonl` | Raw tracing events | ✅ yes |
| OTLP collector (rotel) | Spans, metrics, logs | ✅ yes (in-memory) |

## Quick commands

```bash
# Tail live events (jsonl, pretty-printed)
crabcc track --tail

# Inspect the jsonl file with csvlens
task telemetry-watch

# Inspect any .crabcc SQLite DB with csvlens
task db-inspect DB=.crabcc/index.db

# Enable OTLP export (point to rotel or any collector)
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
crabcc serve   # spans now flow to rotel

# Start rotel alongside the dev stack
task telemetry-rotel

# Enable Telegram notifications for WARN/ERROR + KPI events
export TELEGRAM_BOT_TOKEN=...
export TELEGRAM_CHAT_ID=...
crabcc serve
```

## KPI events (captured at INFO level)

| Target | Event | Fields |
|--------|-------|--------|
| `crabcc_mcp` | `dispatch_tool_with` | `tool`, `elapsed_ms`, `ok\|error` |
| `crabcc_core::graph` | `graph.build` | `edges`, `nodes`, `duration_ms` |
| `crabcc_core::graph` | `graph.cycles` | `count`, `duration_ms` |
| `crabcc_core::graph` | `graph.orphans` | `count`, `duration_ms` |
| `crabcc_core::graph` | `graph.walk` | `direction`, `depth`, `frontier_size` |
| `crabcc_cli::agent` | `sandbox.*` | `x_request_id`, `x_timings`, `cold/warm` |

## Env vars

| Var | Default | Effect |
|-----|---------|--------|
| `RUST_LOG` | `crabcc_mcp=info,crabcc_core::graph=info,warn` | stderr filter |
| `CRABCC_TELEMETRY_LOG` | same as RUST_LOG | jsonl file filter |
| `CRABCC_TELEMETRY_FILE` | `.crabcc/telemetry.jsonl` | jsonl path override |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | unset (disabled) | OTLP HTTP endpoint |
| `CRABCC_ENV` | `local` | `deployment.environment` span attribute |
| `TELEGRAM_BOT_TOKEN` | unset (disabled) | Telegram notifications |
| `TELEGRAM_CHAT_ID` | unset (disabled) | Telegram chat destination |

## OTel resource attributes

Baked into the Docker image labels and set in `init()`:

```
service.name    = "crabcc"
service.version = CARGO_PKG_VERSION
deployment.environment = CRABCC_ENV (default: "local")
```

These are also set as OCI image labels (`org.opentelemetry.*`) in
`install/Dockerfile.crabcc` and `install/Dockerfile.base` so rotel
and any collector auto-tag spans without extra config.

## Key source locations

| File | What |
|------|------|
| `crates/crabcc-cli/src/telemetry.rs` | Init + all 4 layers |
| `crates/crabcc-viz/src/lib.rs:428` | `/api/telemetry` HTTP route |
| `crates/crabcc-viz/src/lib.rs:612` | `telemetry_tail()` jsonl reader |
| `install/Dockerfile.crabcc` | OTel image labels |
| `install/Dockerfile.base` | OTel image labels |
| `Taskfile.yml` | `task telemetry-rotel`, `task telemetry-watch`, `task db-inspect` |
