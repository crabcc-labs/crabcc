# crabcc-viz — `/live` dashboard

Issue #64, #72, #75, #78, #86, #90.  
Source: `crates/crabcc-viz/`

## Architecture

```
crabcc serve --port 8090
  │
  ├─ Rust HTTP server (tiny_http)   crates/crabcc-viz/src/lib.rs
  │     GET /live          → dist/live.html  (bundled via include_str!)
  │     GET /api/events    → SSE stream (activity, agents)
  │     GET /api/agents    → agent list
  │     GET /api/telemetry → .crabcc/telemetry.jsonl tail
  │     GET /api/telemetry/otlp-health → OTLP endpoint TCP probe
  │     GET /api/graph     → call-graph snapshot
  │     GET /api/bootstrap → root, repo, version
  │     POST /api/reindex  → trigger `crabcc index`
  │     … (20+ routes)
  │
  └─ React/TypeScript SPA  crates/crabcc-viz/web/src/
        App.tsx            root — polling + SSE wiring
        api.ts             typed fetch client
        usePolling.ts      interval-based polling hook
        useEventStream.ts  SSE hook (agents, activity)
        components/
          AgentsPanel       agent lifecycle, log viewer
          TelemetryPanel    jsonl KPI stream + OTLP health pill
          SettingsPanel     user-configurable dashboard settings
          Graph             vis-network call graph
          ActivityPanel     random-query burst history
          …
```

## Quick commands

```bash
# Start the dashboard
crabcc serve                   # http://localhost:8090/live

# Dev mode — hot-reload web UI on save
task viz-web-dev               # bun watch + esbuild in container
bun run dev                    # from crates/crabcc-viz/web/

# Build production bundle
bun run build                  # → web/dist/live.html  (include_str!'d at compile time)

# Run React tests
task viz-web-test              # bun test + tsgo typecheck
bun test src                   # from crates/crabcc-viz/web/

# Typecheck only
bun run typecheck

# Rust viz tests
cargo test -p crabcc-viz
```

## API routes (key ones)

| Route | Method | Response |
|-------|--------|----------|
| `/live` | GET | `dist/live.html` (self-contained SPA) |
| `/api/events` | GET | SSE: `activity`, `agents` events |
| `/api/agents` | GET | `{agents: AgentSummary[]}` |
| `/api/telemetry` | GET | `{cursor, events, source}` — jsonl tail |
| `/api/telemetry/otlp-health` | GET | `{reachable, endpoint, error?}` |
| `/api/graph` | GET | `{nodes, edges, …}` — call-graph |
| `/api/bootstrap` | GET | `{root, repo, version}` |
| `/api/reindex` | POST | triggers `crabcc index` |
| `/api/health` | GET | `{status:"ok"}` |
| `/api/random-query` | POST | fires a random sym/refs query |
| `/api/ollama-key` | GET | LiteLLM key status (redacted) |

## Telemetry panel (issues #86 + #90)

Two sections:

1. **OTLP health pill** — probes `OTEL_EXPORTER_OTLP_ENDPOINT` via TCP every N seconds (default 30 s, configurable). Shows green `● OTLP host:port` when reachable, red `● OTLP unreachable` otherwise, grey `○ OTLP disabled` when the env var is not set.

2. **KPI event stream** — tails `.crabcc/telemetry.jsonl`, colour-coded by level. Events sourced from `tracing::info!` through the jsonl layer in `telemetry.rs`.

## Dashboard settings (⚙ gear icon)

Stored in `localStorage` key `crabcc_dashboard_settings`. Applying settings reloads the page so all polling intervals update.

| Setting | Default | Range |
|---------|---------|-------|
| OTLP probe interval | 30 s | 15 s – 1 h |
| Telemetry poll interval | 3 s | 1 s – 60 s |
| Max telemetry events | 100 | 10 – 500 |
| Agent poll interval | 5 s | 2 s – 60 s |

## Storage contract

The viz server reads `.crabcc/telemetry.jsonl` (append-only, written by the tracing pipeline). It NEVER writes to `_internal.db` or `index.db`. Agent-run data comes from `_internal.db` via the Rust side; the React layer receives it over SSE/polling and holds it in-memory only.

## Key source locations

| File | What |
|------|------|
| `src/lib.rs` | HTTP server, all route handlers |
| `src/lib.rs:580` | `otlp_health_probe()` TCP check |
| `src/lib.rs:612` | `telemetry_tail()` jsonl reader |
| `web/src/api.ts` | typed fetch client, all types |
| `web/src/components/TelemetryPanel.tsx` | OTLP pill + KPI stream |
| `web/src/components/SettingsPanel.tsx` | settings modal (localStorage) |
| `web/src/App.tsx` | polling wiring, settings state |
| `web/src/styles.css` | all styles incl. cyberpunk/liquid-glass settings |
