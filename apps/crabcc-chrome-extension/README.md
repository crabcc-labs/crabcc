# crabcc Chrome bridge (MV3 extension)

A Chrome MV3 extension that drives `window.__crabcc__` on the active
tab. Two transports, both speaking the same `RpcRequest` / `RpcResponse`
JSON envelope:

| Transport | Status | Use when |
|---|---|---|
| WebSocket (`ws://localhost:7878/ws/extension`) | default | quick local hacks; the `crabcc serve` dashboard is already running |
| Native messaging (`com.crabcc.chrome`) | recommended | hooking up an MCP client; defends against local port scanning |

Issue #184 phases shipped here: 0 (scaffold), 0.5 (WS transport), 1
(`chrome.debugger` lane), 1.5 (V8 profiling), and the extension half of
the native-messaging hookup that the [`crabcc-chrome`](../../crates/crabcc-chrome/README.md)
crate completes.

## Capabilities

| Lane | Methods |
|---|---|
| Bridge (`window.__crabcc__`) | `navigate`, `goBack`, `goForward`, `click`, `hover`, `type`, `selectOption`, `drag`, `pressKey`, `waitFor`, `ariaSnapshot`, `clickByRef`, `hoverByRef`, `typeByRef`, `buttons`, `perfMemory`, `schema`, `state` |
| Capture | `captureVisibleTab`, `tabInfo` |
| `chrome.debugger` | `debuggerAttach`, `debuggerDetach`, `debuggerEvaluate`, `debuggerConsoleList` / `debuggerConsoleClear`, `debuggerNetworkList` / `debuggerNetworkClear`, `debuggerNetworkBody` |
| V8 profiling | `v8CollectGarbage`, `v8HeapSnapshot`, `v8ProfileStart`, `v8ProfileStop`, `v8Metrics` |

All methods route through one dispatcher in `background.ts`, so
WebSocket-driven, native-messaging-driven, and popup-driven calls share
the same code path and the same per-session counters.

## Build

```bash
cd apps/crabcc-chrome-extension
bun install
bun run build      # → dist/
bun run watch      # rebuild on save
bun test           # 28 tests
bun run typecheck  # strict
```

Output:

```
dist/
├─ manifest.json
├─ background.js     ~24 KB
├─ popup.html / popup.css / popup.js
```

## Load in Chrome (dev)

1. Open `chrome://extensions`.
2. Toggle **Developer mode** (top right).
3. **Load unpacked** → select `apps/crabcc-chrome-extension/dist/`.
4. Copy the 32-character ID (e.g. `kjglmoabnpcdfegihkjlmoabnpcdfeghi`).

## Connect via WebSocket

Default. The popup's transport block points at
`ws://localhost:7878/ws/extension` (the `crabcc serve` dashboard's
endpoint). Click **connect**; the popup shows `connected · X rpcs received`.

## Connect via native messaging (recommended)

```bash
# 1. Build + install the bridge binary
cargo install --path crates/crabcc-chrome

# 2. Pair the extension
crabcc-chrome pair --id <extensionId-from-step-4-above>
# → installs ~/Library/Application Support/Google/Chrome/NativeMessagingHosts/com.crabcc.chrome.json
# → writes ~/.crabcc/chrome.toml with a fresh secret

# 3. Verify
crabcc-chrome status
```

In your MCP client config (Claude Code, etc.):

```json
{
  "mcpServers": {
    "crabcc-chrome": {
      "command": "crabcc-chrome",
      "args": ["serve"]
    }
  }
}
```

In the extension popup:
- Switch transport to **native (crabcc-chrome)**.
- Click **connect**.
- The extension calls `chrome.runtime.connectNative("com.crabcc.chrome")`,
  Chrome launches `crabcc-chrome host`, the host TCP-connects to
  `serve`, MCP tools become live.

## Popup UI cheat-sheet

| Section | Buttons / inputs |
|---|---|
| transport | mode radio (websocket ↔ native), endpoint, connect/disconnect, auto-on-startup |
| navigation | back, forward, navigate-to-URL |
| by selector | click, hover, wait-for, type (with submit checkbox), select-option |
| keyboard | press key |
| aria snapshot | snapshot (summary) |
| capture | screenshot (PNG download), tab info |
| devtools (`chrome.debugger`) | attach, detach, eval, console list/clear, network list/clear |
| v8 / profiling | metrics, gc, cpu start/stop (`.cpuprofile` download), heap snapshot (`.heapsnapshot` download) |

The "last result" pane truncates large payloads; downloads link to the
full blob via Object URLs.

## Permissions in `manifest.json`

| Permission | Why |
|---|---|
| `scripting` | Run `executeScript({world: "MAIN"})` against `window.__crabcc__` |
| `activeTab` | Per-popup-open scoping for the active tab; no broad host permission |
| `tabs` | Resolve `tab.url` / `tab.title` outside the activeTab grant for `tabInfo` and screenshot metadata |
| `storage` | Persist transport config (mode, endpoint, auto-flag) across worker restarts |
| `debugger` | `chrome.debugger` lane — Chrome shows a yellow "extension is debugging" banner while attached |
| `nativeMessaging` | `chrome.runtime.connectNative` (the `crabcc-chrome host` channel) |

No `<all_urls>` host permission. The extension touches only the active
tab the operator points it at.

## Source layout

```
src/
├─ background.ts        service-worker; dispatches popup/transport RPCs to executeScript / capability handlers
├─ bridge-rpc.ts        chrome.scripting.executeScript wrappers (page main world)
├─ bridge-types.ts      shared types — kept in sync with crates/crabcc-viz/web/src/debugBridge.ts
├─ debugger-lane.ts     chrome.debugger console + network buffers + V8 profiling
├─ transport.ts         WebSocket *and* connectNative connection lifecycle
├─ popup.html / .css / .ts
├─ __test_shim.ts       chrome + WebSocket fakes for bun:test
└─ *.test.ts            28 tests across bridge-rpc, transport, debugger-lane
```

## Limitations

- Native messaging on Windows isn't supported by `crabcc-chrome pair`
  yet — drop a manifest under `HKCU\Software\Google\Chrome\NativeMessagingHosts\`
  manually if you need it before the registry path lands.
- Tests don't cover the `connectNative` branch — that requires a real
  Chrome to launch the host process. Smoke-test manually.
- Heap snapshots can be 100s of MB on real pages. The popup's truncated
  result pane shows only the size + chunk count; the download link
  carries the full payload.
