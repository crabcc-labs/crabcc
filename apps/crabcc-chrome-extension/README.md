# crabcc-chrome-extension

MV3 Chrome extension that tethers to a local `crabcc serve` daemon over SSE
and bridges agent tool calls (`cua.*` / `devtools.*` / `page.*`) into the
user's authenticated browser session.

Tracking issue: [#184](https://github.com/peterlodri-sec/crabcc/issues/184) ·
RFC: [`examples/cua-integration/RFC-184-chrome-mv3-bridge.md`](../../examples/cua-integration/RFC-184-chrome-mv3-bridge.md)

## Quick start

```bash
# from repo root
task chrome:install        # bun install
task chrome:build          # → apps/crabcc-chrome-extension/dist/
task chrome:dev            # watch mode

# load unpacked: chrome://extensions → Developer mode → "Load unpacked" → pick dist/
crabcc serve               # daemon must be running on :7878
task chrome:e2e            # local-only smoke (NOT run in CI)
```

## Architecture (Phase 0)

```
crabcc serve (:7878)
        │  SSE downlink (rpc events)
        ▼
   offscreen.html  ←─ chrome.runtime.sendMessage ─▶  service-worker
        │                                                │
        │  POST /api/chrome-bridge/response/:id         │  chrome.scripting.executeScript
        ▲                                                ▼
   crabcc serve                                  content scripts (cua-driver)
```

The `EventSource` lives in `offscreen.html` because MV3 service workers are
killed after ~30 s idle. The offscreen document is exempt from that lifecycle
and holds the connection open indefinitely.

## CI gate

CI runs `task chrome:ci` (typecheck + lint). Real integration tests are
local-only — see `task chrome:e2e`.
