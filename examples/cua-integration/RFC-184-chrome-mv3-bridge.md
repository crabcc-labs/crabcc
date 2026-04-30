# RFC #184 — Unified Chrome MV3 Bridge

> Tracked design doc for [issue #184](https://github.com/peterlodri-sec/crabcc/issues/184).
> This file is the source of truth for the plan; the GitHub issue mirrors it.

## Update — 2026-05-01 — Architecture pivot to SSE through `crabcc serve`

The original design below uses a Rust **Native Messaging Host** as the
bridge between the agent and the extension. After verifying what
`crabcc serve` already exposes (SSE machinery at `/api/events`, tiny_http
on loopback `:7878`), we pivoted to a **pure-extension install** that
tethers to the running `crabcc serve` daemon over loopback HTTP/SSE.

**Rationale and trade-off table:** see
[issue #184 comment](https://github.com/peterlodri-sec/crabcc/issues/184#issuecomment-4356550962).
**Implementation discussion:** see "Implementation decisions" below.

### Pivot summary

- **Drop:** the `crabcc-chrome` Rust binary, the `nativeMessaging` permission,
  and the per-OS native-messaging-host JSON.
- **Add:** a broker module in `crabcc-viz` (`broadcast::channel<JsonRpcRequest>`
  + `oneshot` response map) and two routes — `GET /api/chrome-bridge/sse`
  (downlink) and `POST /api/chrome-bridge/response/:id` (uplink). Origin-
  locked to `chrome-extension://<paired-id>`.
- **MV3 keep-alive:** an **Offscreen Document** holds the long-lived
  `EventSource`. The service worker is killed after ~30 s idle; the
  offscreen doc is exempt from that lifecycle.
- **Daemon expectation:** users run `crabcc serve` in the background. The
  extension popup probes `:7878/api/health` and reports unreachable status.

### Revised Phase 0 (replaces the original "Phase 0 — Stub + pairing")

1. Add `broadcast::channel<JsonRpcRequest>` + `oneshot` response map to
   `crabcc-viz` server state.
2. New routes: `GET /api/chrome-bridge/sse` (downlink), `POST
   /api/chrome-bridge/response/:id` (uplink). Origin-locked to
   `chrome-extension://<paired-id>`.
3. Wire `crabcc-mcp` stdio to push JSON-RPC requests onto the broadcast
   channel and `await` the matching `oneshot`.
4. Scaffold the MV3 extension at `apps/crabcc-chrome-extension/` (Bun +
   esbuild + Biome). `offscreen.html` holds `EventSource`; service worker
   routes by `cua.*` / `devtools.*` / `page.*`; content scripts drive the
   DOM via `chrome.scripting.executeScript`.
5. Smoke: `page.list_pages()` round-trips agent → mcp stdio → `crabcc
   serve` → SSE → offscreen → SW → POST back.

### Implementation decisions

- **Shared schema:** TypeShare deferred until the Rust broker module
  lands. For Phase 0 the protocol shapes are hand-written in
  `apps/crabcc-chrome-extension/src/types/protocol.ts` (JSON-RPC 2.0
  envelope + per-method result types). When we build the broker in
  `crabcc-viz`, we'll annotate the Rust structs with `#[typeshare]` and
  generate the TS counterpart, replacing the hand-written file.
- **CI strategy:** `task chrome:ci` (typecheck + lint via Biome) runs on
  GitHub Actions in `.github/workflows/chrome-extension.yml`,
  path-filtered to `apps/crabcc-chrome-extension/**` so TS-only PRs don't
  fire the 564-test Rust suite. Real e2e (`task chrome:e2e`) is
  **local-only** — launches Chrome with `--load-extension=dist/` against
  a running `crabcc serve`. Mirrors the `service_discovery_e2e.rs`
  pattern (gated behind a feature, default `cargo test` skips it).
- **Permissions** (revised manifest): `offscreen`, `tabs`, `activeTab`,
  `scripting`, `storage` + `host_permissions` for `http://localhost:7878/*`.
  `nativeMessaging` is no longer needed; `debugger` returns in Phase 1
  for the `devtools.*` lane only.
- **Pairing:** the QR-code flow in the original Phase 0 still applies,
  but the secret stored is now an extension-id ↔ daemon token pair (used
  for origin-locking the SSE / response routes), not a native-messaging
  host name.

### Architecture (revised)

```
┌─ crabcc agent (Claude Code via crabcc-mcp stdio) ───────────────┐
│  Calls one of:  cua.*  /  devtools.*  /  page.*  /  inspect.*  │
└──────────────────────┬───────────────────────────────────────────┘
                       │ MCP stdio
                       ▼
        ┌──────────────────────────────────┐
        │ crabcc-mcp (existing)            │
        │  pushes JsonRpcRequest onto      │
        │  broadcast channel; awaits       │
        │  matching oneshot response       │
        └────────────────┬─────────────────┘
                         │ in-process
                         ▼
        ┌──────────────────────────────────┐
        │ crabcc-viz HTTP server (:7878)   │  ← chrome-bridge module added
        │  GET /api/chrome-bridge/sse      │       (downlink, server→ext)
        │  POST /api/chrome-bridge/        │       (uplink,   ext→server)
        │       response/:id               │
        └────────────────┬─────────────────┘
                         │ HTTP loopback (origin-locked)
                         ▼
   ┌──────────────────────────────────────────────────────┐
   │ Chrome MV3 extension                                 │
   │                                                      │
   │  offscreen/offscreen.html   (holds EventSource)      │
   │  background/service-worker.ts (routes by tool prefix)│
   │  content/cua-driver.ts      (chrome.scripting tgt)   │
   │  popup/popup.ts             (status / detach UI)     │
   └──────────────────────────────────────────────────────┘
```

The original section below is preserved as historical context for the
Native Messaging design we considered first.

---

## Summary

Build a single Chrome MV3 extension that fuses three browser-control surfaces
into one wire so crabcc agents can drive a real browser, introspect it, and
attach to DevTools — all under one extension ID and one MCP transport.

Builds on (closed) #140's cua-extension scaffolding. The new work is the
*convergence*: instead of three separate plug-points (cua action lane, raw
MCP, DevTools MCP), expose them through one unified extension that the
crabcc agent talks to via a **single MCP stdio bridge** running in a
native-messaging host.

## Why one extension instead of three

| Today (separated) | With unified extension |
|---|---|
| cua spawns Chromium with Puppeteer/CDP — no extension | One extension installed in the user's daily-driver browser |
| chrome-devtools-mcp connects to a separate Chrome instance via debugger | Same DOM, same session, same logged-in tabs |
| Agent juggles 2-3 MCP servers per task | Agent talks to one server: `crabcc-chrome-mcp` |
| Cookies / auth / extensions don't transfer | User's real browser state is the work surface |

The win is **session continuity**. An agent debugging a logged-in
production app can use cua-style "click the Save button" actions on the
same tab where chrome-devtools-mcp is reading network requests and console
logs, with no copy-paste between Chromium instances.

## Architecture

```
┌─ crabcc agent (Claude Code via crabcc-mcp stdio) ───────────────┐
│                                                                  │
│  Calls one of:  cua.*  /  devtools.*  /  page.*  /  inspect.*   │
└──────────────────────┬───────────────────────────────────────────┘
                       │ MCP stdio
                       ▼
        ┌──────────────────────────────────┐
        │ crabcc-chrome-mcp (Rust binary)  │  ← new crate `crabcc-chrome`
        │                                  │
        │  • MCP server (stdio JSON-RPC)   │
        │  • Native-messaging host (Chrome │
        │    talks to it over stdin/stdout)│
        │  • Routes tool calls → extension │
        └────────────────┬─────────────────┘
                         │ Chrome native-messaging
                         ▼
   ┌──────────────────────────────────────────────────────┐
   │ Chrome MV3 extension (manifest_version: 3)           │
   │                                                      │
   │  background/service_worker.ts                        │
   │  ├─ cua-driver.ts         (click/type/screenshot)    │
   │  ├─ devtools-bridge.ts    (chrome.debugger API)      │
   │  ├─ page-introspect.ts    (DOM/network/console)      │
   │  └─ mcp-router.ts         (dispatches by tool prefix)│
   │                                                      │
   │  content_scripts/                                    │
   │  └─ a11y-tree.ts          (accessibility snapshot)   │
   │                                                      │
   │  options/ + popup/        (paired pairing UI)        │
   └──────────────────────────────────────────────────────┘
```

## Tool surface (MCP)

Three flat namespaces, one MCP server.

### `cua.*` — input/output (action lane)

| Tool | Purpose |
|---|---|
| `cua.click(selector \| coords)` | Click; selector is CSS or A11y role+name |
| `cua.type(text, into?)` | Type into focused input or `into=selector` |
| `cua.scroll(dir, amount?)` | Scroll viewport |
| `cua.screenshot(area?)` | PNG of visible page or selector |
| `cua.wait_for(selector, timeout?)` | Block until a node appears |
| `cua.navigate(url)` | `chrome.tabs.update` |

Mirrors the trycua/cua API surface so existing agent prompts work.

### `devtools.*` — debugger lane (mirrors chrome-devtools-mcp)

| Tool | Backed by |
|---|---|
| `devtools.list_network_requests()` | `chrome.debugger` Network domain |
| `devtools.get_network_request(id)` | Single-request body |
| `devtools.list_console_messages()` | Runtime.consoleAPICalled |
| `devtools.evaluate_script(expr)` | Runtime.evaluate |
| `devtools.lighthouse_audit()` | Lighthouse via `chrome.runtime.sendMessage` |
| `devtools.take_memory_snapshot()` | HeapProfiler.takeHeapSnapshot |
| `devtools.performance_start_trace()` / `_stop_trace()` | Tracing.* |

Same shapes the existing `chrome-devtools-mcp` plugin emits — agent code
that targets that plugin works unchanged.

### `page.*` + `inspect.*` — introspection lane

| Tool | Purpose |
|---|---|
| `page.snapshot()` | DOM snapshot (Playwright-style) |
| `page.a11y_tree()` | Accessibility-tree snapshot (lighter than DOM, better for agents) |
| `page.list_pages()` | All tabs in extension scope |
| `inspect.cookies(domain?)` | `chrome.cookies` |
| `inspect.local_storage(origin?)` | `chrome.scripting` injection |
| `inspect.permissions()` | Reflection of granted permissions |

## Phased plan

### Phase 0 — Stub + pairing (1 week) [priority:medium]

- New crate `crabcc-chrome` (Rust binary; native-messaging host + MCP server)
- Bare-bones MV3 extension scaffolding (`bun + esbuild`, vite as fallback)
- **Pairing flow**: same QR-code mechanism as the Telegram bot (#155).
  Operator runs `crabcc chrome pair`, scans QR with extension popup,
  extension stores native-messaging-host name in `chrome.storage.local`.
- Single-tool smoke: `page.list_pages()` round-trips through the whole stack
- `crabcc install-claude --with-chrome-extension` materializes the manifest +
  native-messaging-host JSON

### Phase 1 — DevTools bridge (1 week) [priority:medium]

- Wire `chrome.debugger` in the service worker
- Implement the eight `devtools.*` tools with shapes byte-identical to
  `chrome-devtools-mcp`
- Acceptance: a Claude Code session using the existing `chrome-devtools-mcp`
  skill works against the new bridge with no prompt changes (zero-cost
  migration)

### Phase 2 — cua action lane (1-2 weeks) [priority:medium]

- Port relevant `trycua/cua` action implementations to TS in the
  content-script layer
- A11y-first selectors (role + name) take priority over CSS — agent prompts
  that say "click 'Save'" should resolve via the a11y tree, not query
  selectors
- Screenshot via `chrome.tabs.captureVisibleTab` for visible area,
  `html2canvas` fallback for selector-bounded areas

### Phase 3 — page/inspect lane + polish (1 week) [priority:low]

- DOM + a11y snapshots
- Cookie / storage introspection (with `--allow-cookies` flag — off by
  default; the extension's permissions surface is the gate)
- Popup UI with: paired-host status, live tool-call counter, "detach
  extension" big red button

### Phase 4 — Distribution (1 week) [priority:low]

- Sign + ship to Chrome Web Store under "crabcc Chrome bridge"
- macOS `Crabcc.app` (#107) gets a "Chrome bridge" tab — install / status /
  native-messaging-host log path
- README + AGENTS.md document the install flow

## Security posture

- **No remote endpoints.** All traffic is loopback (extension ⇆
  native-messaging-host) or process-local (host ⇆ MCP stdio).
- **Permissions principle of least.** Manifest permissions: `debugger`,
  `tabs`, `scripting`, `nativeMessaging`. **Not** `<all_urls>` by default —
  the extension prompts per-domain on first use.
- **Single-host pairing** — same lockdown as the Telegram bot (#155).
  Reject native-messaging connections from any unpaired host.
- **Audit log**: every tool call writes a row to
  `~/.crabcc/chrome-extension.log` (mode 0600). One line per call, no
  payload bodies (privacy + size).
- **Cookies / storage tools off by default**, gated by an explicit
  `crabcc chrome enable-introspection` flag persisted in
  `~/.crabcc/chrome.toml`.

## Open questions

1. **Chromium / Brave / Edge support?** MV3 is portable, native-messaging
   is a Chromium primitive — should work everywhere. Test matrix: Chrome
   stable, Chromium dev, Brave. Skip Firefox (different extension model).
2. **MCP transport: stdio vs WebSocket?** stdio is the MCP standard;
   WebSocket would let the extension be the server and the host be the
   client. stdio wins for crabcc-mcp-server consistency. Keep stdio.
3. **Tooling overlap with chrome-devtools-mcp?** Once Phase 1 lands, the
   agent could use either. Soft-deprecate the old plugin once the new
   bridge proves stable. Don't remove until v3.
4. **macOS LaunchAgent integration?** No — the extension lives in the
   browser; the native-messaging-host launches per Chrome session and dies
   with it. No LaunchAgent needed (unlike `com.crabcc.telegram-bot`).

## Acceptance criteria

- [ ] Phase 0 — `page.list_pages()` returns a list of open tabs over MCP stdio
- [ ] Phase 1 — `chrome-devtools-mcp` skill works against the new extension
      with no prompt changes
- [ ] Phase 2 — agent can complete a "log in to GitHub, find a repo, star
      it" script using only `cua.*` tools
- [ ] Phase 3 — popup shows live tool-call counter; detach button kills the
      connection cleanly
- [ ] Phase 4 — Chrome Web Store listing; `Crabcc.app` ships the bridge tab
- [ ] All four phases honor the security posture (no remote endpoints,
      single-host pairing, audit log, opt-in introspection)

## References

- #140 (closed) — cua-extension scaffolding precedent
- #155 — Telegram bot pairing flow + audit log patterns to mirror
- #107 — `Crabcc.app` parent (Phase 4 plugs in here)
- chrome-devtools-mcp — https://github.com/google/chrome-devtools-mcp
  (the surface to mirror in Phase 1)
- trycua/cua — https://github.com/trycua/cua (action-lane reference)
- MV3 native-messaging —
  https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging
- MCP stdio transport —
  https://modelcontextprotocol.io/docs/concepts/transports

## Out of scope

- Headless / non-extension browser drivers
  (cua-as-Chromium-with-Puppeteer) — separate path; this issue is the
  extension story
- Firefox WebExtension parity (different model; defer)
- Mobile (iOS/Android Chrome don't support MV3 extensions)
- Auto-fill / form filler features beyond `cua.type`
