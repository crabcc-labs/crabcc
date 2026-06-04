# Path A — recorded trace (chrome-devtools-mcp + crabcc-mcp)

> Reference trace from a Claude Code session with both MCP servers
> attached. The exact tool calls + arguments the LLM picked. Reproduce
> by pasting the prompt block at the bottom into a fresh session with
> the two servers loaded.

## Prereqs

```jsonc
// ~/.claude.json
{
  "mcpServers": {
    "crabcc":          { "command": "crabcc", "args": ["--mcp"] },
    // Pin the version (not @latest): npx then resolves once and serves the
    // cached package on every later launch instead of re-fetching it.
    "chrome-devtools": { "command": "npx",    "args": ["chrome-devtools-mcp@1.1.1"] }
  }
}
```

> **Browser provisioning — prebuilt, downloaded once at runtime, never at
> build time.** crabcc itself bundles/builds no browser (the agent-runner
> image is deliberately browser-free; nothing in `build.rs` fetches one).
> `chrome-devtools-mcp` downloads a pinned *Chrome for Testing* build lazily
> on first use into its puppeteer cache (`~/.cache/puppeteer/`, or set
> `PUPPETEER_CACHE_DIR`) and reuses it thereafter — so the browser is a
> one-time on-demand download, not a per-run or build-time cost. To
> pre-provision it in CI/containers, run `npx puppeteer browsers install
> chrome` once into a cached layer.

## Trace

```text
> Open https://github.com/peterlodri-sec/crabcc/pull/108 in Chrome.
> Take a snapshot of the "Files changed" tab and list each modified
> function. For each function, call crabcc.callers with mode=files
> to find which files reference it. Output a Markdown table.

[host] new_page                      → tab id 1
[host] navigate_page url=…/pull/108  → loaded (3.4 s)
[host] click selector="a[href$='/files']"   → diff view
[host] take_snapshot scope=accessibility    → 38 nodes
[host] (parses snapshot — 4 changed .rs files, 6 added/changed pub fns)
[host] crabcc.callers name=telemetry_tail mode=files          → ["crates/crabcc-viz/src/lib.rs"]
[host] crabcc.callers name=parse_iso8601_unix mode=files      → ["crates/crabcc-viz/src/lib.rs"]
[host] crabcc.callers name=fmtFields mode=files               → []     # new fn, not yet referenced
[host] crabcc.callers name=fmtAge mode=files                  → []     # same
[host] crabcc.callers name=TelemetryPanel mode=files          → ["crates/crabcc-viz/web/src/App.tsx"]
[host] crabcc.callers name=DebugPanel mode=files              → ["crates/crabcc-viz/web/src/App.tsx"]
[host] (writes Markdown table)

| function              | file                                 | callers (file count) |
|-----------------------|--------------------------------------|----------------------|
| telemetry_tail        | crates/crabcc-viz/src/lib.rs         | 1 (lib.rs)           |
| parse_iso8601_unix    | crates/crabcc-viz/src/lib.rs         | 1                    |
| fmtFields             | web/src/components/TelemetryPanel.tsx| 0 (new — internal)   |
| fmtAge                | web/src/components/TelemetryPanel.tsx| 0 (new — internal)   |
| TelemetryPanel        | web/src/components/TelemetryPanel.tsx| 1 (App.tsx)          |
| DebugPanel            | web/src/components/DebugPanel.tsx    | 1 (App.tsx)          |
```

## Things to notice

- **Latency**: ~6 seconds end-to-end on Sonnet 4.6 with both MCP
  servers warm. Most of it is `navigate_page` + `take_snapshot`;
  `crabcc.callers` rounds at single-digit ms each.
- **Tool routing**: the LLM never confused `crabcc.callers` with a
  Chrome devtools call. The MCP descriptions are token-shaped enough
  (`"name of the function whose call sites you want"`) that no
  retries happened.
- **Zero plumbing**: no Python script, no Python venv, no cua
  install. Two MCP servers + a prompt.
- **Telemetry passthrough**: every `crabcc.callers` call lands as a
  KPI event in `<root>/.crabcc/telemetry.jsonl` — visible in real
  time on the `/live` dashboard's telemetry panel (issue #90).

## When this isn't enough → switch to Path B

Path A doesn't reach outside the browser tab. If your task needs:

- driving an IDE / terminal / file manager,
- multi-window orchestration on macOS,
- background apps that don't have a web equivalent,

the `chrome-devtools-mcp` server can't help — that's the cua slot.
See [`README.md`](README.md) Path B for the Python flow.
