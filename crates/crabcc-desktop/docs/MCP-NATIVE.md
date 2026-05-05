# MCP-Native Desktop — Design

> Status: **draft / planning**. Companion to `DESIGN.md` (the
> existing engineering overview). This doc covers the architecture
> decisions for making the desktop a first-class MCP citizen — host,
> server, and internal bridge.

## 1. Vision (one sentence)

The crabcc desktop is *both* an **MCP host** (it consumes external
servers) *and* an **MCP server** (it publishes its UI, state, and
side-effects). The renderer↔core boundary is itself MCP-shaped, so
nothing in the UI happens that isn't a tool call, a resource read, or
a prompt invocation. The headline view is a live **call inspector**
— a packet sniffer for the agent layer.

## 2. The Inspector (north-star view)

Replaces today's `system` route with a two-pane waterfall.

### 2.1 Data model

Each MCP message — inbound or outbound, internal or external —
becomes a `CallEvent`:

```rust
struct CallEvent {
    id:           Ulid,                 // monotonic, sortable
    ts:           OffsetDateTime,
    server:       ServerId,             // host-side handle
    agent_origin: AgentId,              // "self" for internal
    direction:    Direction,            // In | Out
    method:       SmolStr,              // "tools/call", "resources/read", …
    tool_name:    Option<SmolStr>,      // unwrapped from params for tools/call
    params_ref:   PayloadRef,           // BLAKE3 → content-addressed blob
    result_ref:   Option<PayloadRef>,
    status:       Status,               // Pending | Ok | Err(code)
    latency_ms:   Option<u32>,
    parent_id:    Option<Ulid>,         // causality
    invalidated:  Vec<ResourceUri>,     // declared by the tool's response
}
```

### 2.2 Storage

Two tiers:

- **Ring buffer** (last N = 10 000 events) — in-memory, indexed for
  fast filter/scroll. Lives in the renderer process.
- **Durable log** — append-only sqlite on the core side
  (`.crabcc/desktop/calls.db`), payloads stored content-addressed in
  a blob store so identical args/results dedupe to one row. Retained
  for the configured replay window (default 7 days).

### 2.3 Wire format

NDJSON over an in-process broadcast channel; the inspector subscribes
on mount. The same stream is published externally as the
`desktop://mcp/calls` MCP resource (with subscribe support) so a
second crabcc desktop, or Claude Code, can mirror it.

### 2.4 Affordances

- **Diff** — JSON-patch between params (or results) of two selected
  rows; renders inline with a structural editor.
- **Replay** — re-issue the call as a fresh event tagged
  `replay_of: <id>`. Side-effects pass through the consent gate.
- **Pin** — write to `scenarios.toml`; later promoted into an
  integration-test fixture.
- **Causality tree** — right pane shows `parent_id`-rooted subtree of
  any selected row. Catches "one click → 47 tool calls" pathologies.

## 3. The self-exposed surface

If the inspector renders the stream, the desktop must publish it.
Surface falls into four buckets:

### 3.1 Tools

| Name | Purpose | Side-effects |
|---|---|---|
| `desktop.route.show` | Navigate UI to a route | `desktop://routes/current` |
| `desktop.agent.spawn` | Start an agent | `desktop://agents/*` |
| `desktop.agent.kill` | Stop an agent | `desktop://agents/<id>` |
| `desktop.notify` | Trigger a native notification (rich, see roadmap) | none |
| `desktop.memory.{remember,search,list,get}` | Memory drawer ops | `desktop://memory/*` |
| `desktop.command.run` | Execute a *whitelisted* CLI command | `desktop://logs/<id>` |
| `desktop.window.{focus,minimize}` | Window management | none |

Every write-tool returns `{ ok, side_effects: [ResourceUri], consent_id }` so the
inspector can render blast radius and clients can re-fetch.

### 3.2 Resources

- `desktop://routes/current` — the active route + state snapshot.
- `desktop://agents/{id}` — per-agent live state; subscribable.
- `desktop://logs/{stream_id}` — raw log streams; subscribable.
- `desktop://memory/{drawer_id}` — memory drawer payloads.
- `desktop://mcp/calls` — the call stream itself (meta).
- `desktop://mcp/servers` — registry view; subscribable.

### 3.3 Prompts

Command-palette entries are MCP prompts with arg slots. Letting
external agents enumerate them gives them a discoverable command
surface without us hand-writing tool wrappers.

### 3.4 Sampling offer (the killer feature)

> **What is sampling?** In MCP terminology, an MCP *server* can ask
> its *host* to run an LLM completion on its behalf via
> `sampling/createMessage`. The server doesn't own an API key — the
> host fulfils the request and returns the completion. The desktop
> uses this to make every connected agent and external server
> credential-free.

The plumbing already exists; this section is about *exposing* it
through MCP, not building it:

- `install/ollama-stack/` — `docker-compose.yml` runs LiteLLM as an
  OpenAI-compatible proxy in front of host Ollama, with Caddy in
  front for auth. Default model `qwen2.5-coder` (Apple-Silicon
  friendly).
- `crates/crabcc-core/src/ollama_stack.rs` — `ensure_up()`,
  `check_docker()`, `Options`. As of commit `1ccac779` the stack
  comes up by default; opt out with `--no-docker`.
- `crates/crabcc-cli/src/agent_bullmq.rs` — `BullmqRuntime` already
  enqueues agent runs into a queue whose workers spawn sandboxed
  Docker (or Apple `container`, per issue #112) containers.

The MCP-native angle is to wire `sampling/createMessage` into the
LiteLLM endpoint so any connected MCP server (slack, github, the
agent containers themselves) can request inference *through the
desktop* without holding credentials.

`servers.toml`:

```toml
[sampling]
backend  = "litellm"                 # in front of ollama
endpoint = "http://127.0.0.1:4000"   # litellm proxy
model    = "ollama/qwen2.5-coder"    # default; client may request another
gate     = "allow-trusted"           # see §5; trust set is small (iPhone + local containers)
```

Each external sampling request:

1. Passes the consent gate (allow-trusted skips the toast for
   pre-declared peers; everything else prompts).
2. Logs as a `CallEvent` with `method = "sampling/createMessage"` and
   the chosen model in `params_summary`.
3. Returns the completion, or a typed `sampling_unavailable` error
   so clients can fall back.

**Sampling for containerised agents.** The loop:

```
agent (in container) ──MCP──▶ desktop (host) ──HTTP──▶ litellm ──▶ ollama
                       ◀──── completion ────────────────────────────────
```

Net effect: agents launched via `BullmqRuntime` never carry their
own model credentials. They speak MCP back to the desktop (over the
container↔host bridge — Unix socket bind-mount or VSOCK on Apple
`container`) and request sampling. Every inference is one row in the
inspector, attributable to the originating agent.

This is also why the desktop *is* a peer to Claude Code rather than
a competitor: paste `crabcc desktop` into Claude Code's MCP config
and Claude Code can sample locally for cheap drafts before paying
for cloud calls.

## 4. The host architecture

### 4.1 Process model

| Server kind | Transport | Why |
|---|---|---|
| External (slack, github, fff…) | stdio child | standard MCP, swap-in |
| crabcc's own server | in-proc (Rust crate, MCP-shaped) | zero serialization on the hot path |
| Trusted local (shared with Claude Code) | Unix socket | one server, many agents |

The renderer↔core internal bridge speaks MCP *shape* (same method
names, same `CallEvent` rendering) but uses postcard encoding instead
of JSON. 60 fps interactions stay direct fn calls; everything
user-meaningful round-trips through MCP.

### 4.2 Registry

`~/.crabcc/desktop/servers.toml` — declarative, hot-reloadable,
surfaced as a route:

```toml
[server.slack]
command   = "npx"
args      = ["-y", "@modelcontextprotocol/server-slack"]
env       = { SLACK_TOKEN = "$KEYCHAIN:slack" }
roots     = []
autostart = true
sandbox   = "default"

[server.fff]
command   = "fff"
args      = ["mcp"]
roots     = ["~/workspace"]
autostart = true
sandbox   = "default"

[server.crabcc]
kind      = "in-proc"
roots     = ["$CWD"]
sandbox   = "none"   # in-proc, can't sandbox itself
```

### 4.3 Lifecycle

A supervisor task per server: spawn → ready-handshake → health-ping
loop → restart-with-backoff on crash → structured shutdown on quit.
Every state transition is itself a `CallEvent` (`server/up`,
`server/down`, `server/health_warn`) so the inspector shows liveness
in band.

### 4.4 Sandbox profiles (macOS first)

**Trust boundary.** External MCP peers are limited to:

1. **The owner's iPhone**, reached over Tailscale or
   Bonjour-on-LAN. Pre-paired; identity = device cert pinned at
   pairing time.
2. **Local containers** running on the same Mac (Docker, Apple
   `container`). Reached over Unix socket bind-mount or VSOCK; the
   socket itself is the capability — only processes inside our
   spawned containers can see it.

There is **no public network exposure** and no story for arbitrary
remote peers. This narrows §5 consent considerably: the threat model
is "a bug in our agent code does something bad", not "an attacker
on the internet".

- `default` — for stdio child servers: `sandbox-exec` profile,
  read-only fs except per-roots, network allowlist, no
  `~/Library` access.
- `container` — for agent runs delegated to `BullmqRuntime`: the
  Docker / Apple `container` profile *is* the sandbox; we just pass
  through stdio.
- `unrestricted` — opt-in, banner badge in the registry UI.
- `none` — only valid for in-proc; logged warning otherwise.

(Linux: bubblewrap; Windows: AppContainer — out of scope for v1.)

## 5. Consent model

Given the narrowed trust boundary (§4.4 — only iPhone + local
containers), consent is mostly about *visibility and revocability*,
not adversarial defence.

Each `(server × tool)` pair has one of four modes:

- **allow-trusted** — default for the user's paired iPhone and for
  containers spawned by `BullmqRuntime` (their socket *is* the
  capability). Logged, never prompts.
- **prompt** — transient toast: *allow once / allow for session /
  deny*. Logged in the inspector.
- **allow-implicit** — declared trusted in `servers.toml` for
  ad-hoc additions.
- **deny** — never invokable; calls return a typed error.

Long-press any tool row in the inspector → revoke its session grant.
Consent decisions are themselves resources
(`desktop://consent/grants`) so a future "audit" view writes itself.

## 6. Hot path / cold path boundary

Things that **must not** go through MCP:

- Per-frame redraw, scroll, hover.
- Animation timelines.
- Text-input keystroke handling (latency budget < 4 ms).

Things that **must** go through MCP:

- Any state change a user can describe ("spawn agent", "open file",
  "search memory").
- Any resource a debugger would want to replay.
- Anything cross-process or cross-machine.

Rule of thumb: if a feature would benefit from being scriptable,
recordable, or remote-drivable, it's MCP. If the answer is "no, it's
just paint", it's a direct call.

## 7. Open questions

- **Capability negotiation** — we'll require modern MCP clients
  (resource subscriptions, sampling). Do we publish a minimum
  protocol version?
- **Cost accounting** — sampling tokens consumed per external server;
  does the inspector show running total?
- **Multi-window** — one host per window or one per app? Current
  lean: one per app, windows share the call stream.
- **Replay determinism** — replays of tools that read live state
  (e.g. `agent.spawn`) won't match. Mark non-deterministic tools in
  the registry so the inspector renders replay-vs-original
  divergences as expected, not errors.
- **Persistence of pinned scenarios** — auto-promote into
  `tests/scenarios/` or keep as user-private state?

## 7a. Glossary

- **Sampling** — MCP message kind `sampling/createMessage`. A
  *server* asks its connected *host* to run an LLM completion. The
  desktop fulfils these via the existing LiteLLM→Ollama stack so
  external servers and containerised agents never need their own
  API keys.
- **Replay** — re-issuing a recorded `CallEvent` from the inspector.
  *Read-replay* is idempotent (re-fetch a resource, diff the
  result). *Write-replay* re-fires a side-effecting tool call
  (`agent.spawn`, `notify`, `command.run`) gated by a fresh consent
  prompt. Replays carry `replay_of: <id>` so the inspector renders
  them next to the original with a divergence diff.
- **Roots** — the MCP concept of "filesystem roots a server is
  allowed to see". Surfaced as a per-server picker in the registry
  route; revocable.
- **`CallEvent`** — our internal record of one MCP message
  (in or out, internal or external). The inspector renders the
  stream of these.

## 8. What this *isn't*

- A general-purpose MCP IDE (we're not building Cursor's tool panel).
- A replacement for Claude Code (we're a peer; we expose ourselves
  to it).
- A protocol fork (we follow upstream MCP; any extension is
  proposed upstream first).

## 9. Roadmap sketch

1. **M0 — call recorder.** In-proc bridge already exists; instrument
   it to emit `CallEvent`s into a ring buffer. Inspector route
   reads the buffer. No external servers yet.
2. **M1 — first external server.** Spawn `fff` over stdio; route its
   tools through the same `CallEvent` pipeline. Registry UI is
   read-only.
3. **M2 — desktop-as-server.** Publish tools/resources/prompts.
   Drive desktop from Claude Code; eat dogfood.
4. **M3 — sampling offer.** Local Ollama as sampling provider. This
   is the marketing-grade demo.
5. **M4 — consent + sandbox.** Don't ship M2 to non-developers
   without it.
6. **M5 — replay + pinned scenarios.** Becomes the regression-test
   harness for the desktop itself.

---

Companion docs to write next (each becomes its own file under
`docs/` once we agree on shape):

- `MCP-INSPECTOR.md` — wire format + storage layout in detail.
- `MCP-SAMPLING-OFFER.md` — the Ollama-as-provider design.
- `MCP-CONSENT.md` — security model + threat scenarios.
