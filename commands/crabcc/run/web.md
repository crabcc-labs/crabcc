---
description: Start the crabcc /live web dashboard locally — builds the React bundle, ensures the index/graph sidecars exist, then launches `crabcc serve` and opens the browser.
---

Start the crabcc `/live` web dashboard for the current repo.

## Steps

1. Ensure the index + graph sidecars exist (cheap when already built):
   ```
   test -f .crabcc/index.db || crabcc index
   test -f .crabcc/graph.json || crabcc graph build
   ```
2. Build the React bundle into `crates/crabcc-viz/web/dist/live.html` if
   missing or stale (Taskfile handles freshness):
   ```
   task viz-web-build
   ```
3. Launch the dashboard. Default port is 7878; pass `PORT=NNNN` to
   override. Pass `NO_OPEN=1` to skip the auto-browser-open (useful over
   SSH or in a test loop):
   ```
   task viz
   ```
   The dashboard streams tool calls, agent runs, telemetry, services,
   and OTLP health via Server-Sent Events. The "live" pill in the
   header binds to the SSE connection state.

## Optional: dev-mode hot reload

For frontend iteration on `crates/crabcc-viz/web/src/`, run the
esbuild watcher in a separate terminal so saves rebuild
`dist/live.html`:

```
task viz-web-dev
```

Cmd-R in the browser picks up the new bundle.

## Troubleshooting

- **`task: command not found`** — install [Task](https://taskfile.dev)
  via `brew install go-task/tap/go-task` (macOS) or follow the
  upstream install guide.
- **Port 7878 in use** — `PORT=8787 task viz` or kill the existing
  server: `lsof -nP -iTCP:7878 | awk 'NR>1 {print $2}' | xargs kill`.
- **Empty panels / "no telemetry yet"** — the dashboard reads from
  `.crabcc/telemetry.jsonl`. Run any `crabcc graph …` query (or use
  the MCP server) to populate it.
- **DevTools console silent** — open the console; lifecycle logs
  (`[crabcc] …`) emit on each panel mount, fetch result, and SSE
  state transition. Set `localStorage.crabcc_silent = "1"` to mute.
