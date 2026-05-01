# `crabcc-chrome`

Native-messaging host + stdio MCP bridge for the crabcc Chrome extension
(`apps/crabcc-chrome-extension`). Replaces the Phase-0.5 loopback
WebSocket transport with Chrome's native-messaging channel, which is
process-local, host-name-pinned, and not visible to other local
processes.

> Status: Phase-1 of [issue #184](https://github.com/peterlodri-sec/crabcc/issues/184).
> The crate ships behind the same workspace `cargo build` as the rest of
> crabcc; no extra runtime dependencies.

## Architecture

```
┌─────────────────────────┐
│ MCP client              │ Claude Code, Cursor, etc.
│ (e.g. claude-code)      │
└────────┬────────────────┘
         │ stdio JSON-RPC 2.0 (MCP)
         ▼
┌─────────────────────────┐
│ crabcc-chrome serve     │ long-lived; bound to 127.0.0.1:0
└────────┬────────────────┘
         │ TCP loopback (newline-JSON, secret-authenticated)
         ▼
┌─────────────────────────┐
│ crabcc-chrome host      │ launched by Chrome per `connectNative` call
└────────┬────────────────┘
         │ Chrome native messaging (4-byte LE length + JSON body)
         ▼
┌─────────────────────────┐
│ Chrome MV3 extension    │ apps/crabcc-chrome-extension
└─────────────────────────┘
```

A single `crabcc-chrome` binary, three modes:

| Mode | Launched by | Stdio role |
|---|---|---|
| `host` | Chrome | Native-messaging frames |
| `serve` | MCP client | MCP JSON-RPC 2.0 |
| `pair` / `unpair` / `status` | Operator | Plain stdout |

`serve` opens a TCP loopback listener so it can receive the host's
relayed traffic. Chrome can't be used to bridge MCP and native-messaging
in the same process — the two would fight over stdin/stdout — so two
processes share state via a loopback socket.

## Install

```bash
# from the repo root
cargo install --path crates/crabcc-chrome
```

The binary is named `crabcc-chrome`.

## Pair

1. Build the extension and load it via `chrome://extensions` ▸ "Load
   unpacked" pointing at `apps/crabcc-chrome-extension/dist/`.
2. Copy the 32-character extension ID shown on the extensions page.
3. Run:

   ```bash
   crabcc-chrome pair --id <extensionId>
   ```

   This:
   - Writes `~/Library/Application Support/Google/Chrome/NativeMessagingHosts/com.crabcc.chrome.json`
     (or the Linux equivalent under `~/.config/google-chrome/...`)
   - Writes `~/.crabcc/chrome.toml` with a fresh 32-byte secret + the
     extension ID. Mode `0600`.
4. Other Chromium-family browsers: `--browser chromium | brave | edge`.
5. Re-run with `--force` to refresh the secret or overwrite the manifest.

Verify:

```bash
crabcc-chrome status
# host name:        com.crabcc.chrome
# manifest:         /Users/.../com.crabcc.chrome.json
# manifest exists:  true
# config:           /Users/.../.crabcc/chrome.toml
# secret set:       true
# listen port:      0
```

`listen port: 0` until `serve` runs.

## Use

Start `serve` from your MCP client config:

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

Then in the extension popup, switch transport mode to **native
(crabcc-chrome)** and click **connect**. The extension calls
`chrome.runtime.connectNative("com.crabcc.chrome")`, Chrome launches
`crabcc-chrome host`, the host TCP-connects to `serve`, the MCP client
gets a tool list mirroring the extension's bridge methods.

## MCP tools

`tools/list` returns one tool per bridge method, prefixed `browser_`:

```
browser_navigate           browser_ariaSnapshot
browser_click              browser_clickByRef
browser_type               browser_typeByRef
browser_hover              browser_hoverByRef
browser_drag               browser_pressKey
browser_selectOption       browser_waitFor
browser_captureVisibleTab  browser_tabInfo
browser_debuggerAttach     browser_debuggerEvaluate
browser_debuggerConsoleList    browser_debuggerNetworkList
browser_debuggerNetworkBody    browser_debuggerNetworkClear
browser_v8CollectGarbage   browser_v8HeapSnapshot
browser_v8ProfileStart     browser_v8ProfileStop
browser_v8Metrics          browser_schema
browser_state              browser_buttons
browser_perfMemory         browser_goBack / browser_goForward
                           browser_debuggerDetach
                           browser_debuggerConsoleClear
```

`tools/call` translates the call into the extension's `RpcRequest`
envelope. Arguments are the tool's `arguments` object, passed
positionally as the first arg of the bridge method (every multi-arg
bridge method already accepts an `opts` object).

## Security posture

- **No remote endpoints.** `serve` binds 127.0.0.1; the host process
  speaks only to that loopback address.
- **Host-pinning.** The Chrome manifest's `allowed_origins` list is
  pinned to exactly one extension ID at pair time. Chrome rejects
  `connectNative` from any other extension.
- **Secret-authenticated socket.** Every host-→-`serve` connection
  presents the 32-byte hex secret from `chrome.toml`. Anything else
  (including future `serve` instances on the same loopback port range)
  is rejected before any traffic flows.
- **Single host per `serve`.** A second host connection while one is
  active is rejected.
- **Wire-version handshake.** Every connection includes
  `wireVersion: 1`; mismatch closes the connection so a host built
  against a different envelope can't write malformed traffic into the
  bridge.
- **No `<all_urls>` in the extension.** The native-messaging permission
  alone doesn't grant page access; the extension still uses
  `activeTab` + per-tab debugger attach.

## Threat model

| Threat | Mitigation |
|---|---|
| Local malicious process scans loopback ports | Secret in `chrome.toml`; `serve` rejects unauth'd connections |
| A malicious extension calls `connectNative` | `allowed_origins` pinned at pair |
| Another local user reads `chrome.toml` | Mode `0600` on Unix |
| Wire format drift between host and `serve` | `wireVersion` handshake |
| Stdout pollution breaking native messaging | All `tracing` output goes to stderr |
| Host left attached after the user navigates away | Chrome auto-detaches debugger on tab close (handled by `debugger-lane.ts`) |

What this **doesn't** protect against:

- Compromise of the user's account — `chrome.toml` is reachable from any
  process running as that user. Same threat surface as `~/.aws/credentials`.
- A malicious extension installed at the same time as the legit one
  with a known-target ID. Loading unpacked extensions still warns; the
  Chrome Web Store review is the gate in production.

## Configuration

`~/.crabcc/chrome.toml` (or `$CRABCC_CHROME_CONFIG`):

```toml
port = 0          # written by `serve` on startup
secret = "..."    # 32-byte hex; written by `pair`
extension_id = "abcdefghij..."  # 32-char Chrome extension ID
```

`port = 0` means "no `serve` is currently up" — the host refuses to
launch in that case.

## Development

```bash
cargo test -p crabcc-chrome              # 11 unit tests
cargo build -p crabcc-chrome             # debug build
cargo build -p crabcc-chrome --release   # release build
CRABCC_CHROME_LOG=trace crabcc-chrome serve   # verbose tracing
```

Bumping `WIRE_VERSION` in `lib.rs` requires a matching bump in the
extension's transport handshake; otherwise `serve` rejects host
connections with a `wireVersion mismatch` error.

## Limitations

- Windows registry-based manifest install isn't implemented (`pair` errors
  out on non-Unix). Drop a manifest manually under
  `HKCU\Software\Google\Chrome\NativeMessagingHosts\com.crabcc.chrome`
  in the meantime.
- One Chrome host per `serve`. Multi-tab support requires a session ID
  on the wire envelope and per-session dispatch — deferred.
- The MCP layer implements `initialize`, `tools/list`, `tools/call`. No
  resources, prompts, sampling, or notifications yet.
