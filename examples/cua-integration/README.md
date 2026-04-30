# crabcc + cua / Chrome-DevTools-MCP — browser-driven code review

> Use case for [issue #107](https://github.com/peterlodri-sec/crabcc/issues/107) Part B —
> Chrome-extension agent. **Two paths shown side-by-side**, both grounded in
> crabcc's MCP symbol surface:
>
> | Path                          | Status                                       | Best for                                  |
> |-------------------------------|----------------------------------------------|-------------------------------------------|
> | **A. Chrome DevTools MCP**    | already shipped in Claude Code — use today   | browser-only flows (PR review, scrape, fill) |
> | **B. trycua/cua**             | Python SDK — separate install                | desktop-wide computer use (native apps)   |
>
> Path A is the immediate-unblock for #107 Part B. Path B is the broader
> "drive any app on the desktop" track. Pick the one that matches your scope.

## Why pair them

Each tool covers exactly the half the other doesn't:

| Surface              | Owns                                                 |
|----------------------|------------------------------------------------------|
| **trycua/cua**       | UI / browser / desktop automation; runs models locally; doesn't know what your code means |
| **crabcc** (MCP)     | Symbol-aware code lookups — `sym`, `refs`, `callers`, `outline`; doesn't drive UI |

In a code-review workflow, cua reads the PR diff (the *what*) and crabcc
explains the surrounding code (the *why* — who calls this function, where is
it defined, what's its parent, is it on a hot path).

## Architecture

### Path A — Chrome DevTools MCP (no extra install)

```
┌──────────────────────────┐    JSON-RPC    ┌──────────────────────────┐
│ Claude Code (host LLM)   │ ◄──────────►   │ chrome-devtools-mcp      │
│ ───────────────────────  │                │ ───────────────────────  │
│ * loads BOTH MCP servers │                │ * navigate_page          │
│   as plugin tools        │                │ * take_snapshot          │
└─────────┬────────────────┘                │ * click / fill / scroll  │
          │                                 │ * list_network_requests  │
          │ JSON-RPC                        └──────────────────────────┘
          ▼
┌──────────────────────────┐
│ crabcc --mcp             │
│ ───────────────────────  │
│ * sym / refs / callers   │
│ * outline / files / grep │
│ * memory.* drawer store  │
└──────────────────────────┘
```

The host (Claude Code, or any MCP client) holds both server connections
open and routes tool calls by name. **No subprocess wiring on your end.**
This is the path the Chrome-extension prereq for #107 lands on first.

### Path B — trycua/cua (Python SDK, native-app reach)

```
┌──────────────────────────┐                ┌──────────────────────────┐
│ cua-agent (Python SDK)   │   MCP/stdio    │ crabcc --mcp             │
│ ───────────────────────  │ ─────────────► │ ───────────────────────  │
│ * model: ollama qwen3.5  │                │ * sym / refs / callers   │
│ * tool: computer-use     │                │ * outline / files / grep │
│ * tool: crabcc-mcp ◄─────┘                │ * memory.* (drawer store)│
│ * tool: browser-driver                    └──────────────────────────┘
│                                                                       │
│   cua spawns crabcc --mcp as a child; JSON-RPC over the child's      │
│   stdio. Same wire format as Path A — only the host changes.         │
└──────────────────────────────────────────────────────────────────────┘
```

Same JSON-RPC contract. cua adds **native app control** — opens IDEs,
terminals, file managers, anything macOS exposes via the Accessibility
API. Chrome DevTools MCP is browser-only.

Two key ergonomics:

1. **No extra wire format.** cua's tool surface is JSON-RPC; crabcc
   already speaks JSON-RPC over stdio (`crabcc --mcp`). They're a 1-line
   subprocess spawn apart.
2. **Local-first.** Both run on `127.0.0.1`. The Ollama backend (per
   `task ollama-bootstrap`) is the only LLM in the loop. Zero cloud round-trips.

## Path A — Chrome DevTools MCP (try this first)

If you're running this from Claude Code, both servers are one config-file
edit apart. Add to `~/.claude.json` → `mcpServers`:

```jsonc
{
  "mcpServers": {
    "crabcc": { "command": "crabcc", "args": ["--mcp"] },
    "chrome-devtools": { "command": "npx", "args": ["chrome-devtools-mcp@latest"] }
  }
}
```

(or run `crabcc install-claude` once, then add the chrome-devtools entry.)

Then in Claude Code, with both MCP servers loaded, paste this prompt:

> Open https://github.com/peterlodri-sec/crabcc/pull/108 in Chrome.
> Take a snapshot of the "Files changed" tab and list each modified
> function. For each function, call the `crabcc.callers` tool with
> `mode=files` to find which files reference it. Output a Markdown
> table: function | file | callers (file count).

The host LLM (Claude in Claude Code) interleaves
`chrome_devtools.navigate_page` / `take_snapshot` calls (cookies + DOM
read) with `crabcc.callers` / `crabcc.outline` calls (code grounding).
**No extra Python / subprocess plumbing.** See `path-a-chrome-mcp.md`
in this directory for a recorded trace.

## Path B — cua (Python, native-app reach)

```bash
# 1. Prereqs.
task ollama-bootstrap                 # Ollama + the recommended NVFP4 model
cargo install --path crates/crabcc-cli --locked   # crabcc + ccc binaries
brew install python                   # 3.11+

# 2. cua-agent SDK.
python3 -m venv .venv && source .venv/bin/activate
pip install cua-agent cua-computer-server

# 3. Run the example. Substitute a real PR URL on a repo you've indexed.
crabcc index                          # one-time, in the target repo
python3 examples/cua-integration/cua-with-crabcc.py \
    --pr "https://github.com/peterlodri-sec/crabcc/pull/108" \
    --root "$(pwd)"
```

What the script does (~30 s end-to-end on M-series + Ollama):

1. Spawns `crabcc --mcp` as a child process (stdio JSON-RPC).
2. Boots a `cua-agent` configured with two tool sources:
   * **cua's `computer.tool`** — controls a sandboxed Chromium tab.
   * **A `MCPToolAdapter`** wrapping the crabcc MCP server.
3. Hands the agent the same prompt as Path A.
4. The agent alternates between `computer.click(...)` (cua) and
   `mcp.callers(name="...")` (crabcc) — until it has the data, then writes
   the summary to stdout.

## What the example demonstrates

- **MCP-as-tool source** — any cua agent can pick up the entire crabcc
  surface (today: 14 tools) by spawning one subprocess. No HTTP server
  to stand up.
- **Tool-routing**: cua's executor picks the right tool by description.
  crabcc's MCP descriptions are token-shaped (per `examples/MCP.md`)
  so an Ollama-driven agent picks the right surface without retries.
- **Telemetry passthrough**: every `crabcc.<tool>` call lands in
  `<root>/.crabcc/telemetry.jsonl` — the same file the `/live`
  dashboard's telemetry panel reads (issue #90). The dashboard
  *renders* the cua agent's actions live, even though cua doesn't
  know the dashboard exists.

## Browser-side equivalent (Chrome extension, issue #107 Part B)

The Chrome extension this example previews wraps the same flow in:

- A side panel that takes a prompt + the active tab's URL.
- A service worker that spawns `crabcc --mcp` via cua's `cua-driver`
  binary (already managing the user's Ollama stack).
- A WebSocket bridge between the panel and the worker.
- The same MCP tool adapter — wrapped in `chrome.runtime` instead of
  Python's `subprocess`.

Anything the Python script can do here, the extension can do via the
same JSON-RPC contract. That's deliberate: the test in this dir is
the extension's headless harness.

## Files in this directory

| File                       | What it is                                          |
|----------------------------|-----------------------------------------------------|
| `README.md`                | this file                                           |
| `cua-with-crabcc.py`       | the runnable example                                |
| `prompt.md`                | the system prompt the agent gets — edit to change task scope |
| `mcp-tool-adapter.py`      | small `subprocess.Popen` wrapper that adapts crabcc's MCP to cua's `Tool` ABC |
