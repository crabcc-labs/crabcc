# MCP Sampling Offer — Protocol Spec

> Companion to `MCP-NATIVE.md` §3.4. This doc commits to wire-format
> details for the desktop's sampling-offer: how connected MCP servers
> (and containerised agents) ask the desktop to run an LLM completion,
> how the request is routed through the existing LiteLLM→Ollama
> stack, and how it's audited.
>
> Status: **draft**. Targets M3 of the roadmap in `MCP-NATIVE.md` §9.

## 1. Goal in one sentence

Make every connected MCP peer credential-free for inference: the
peer issues `sampling/createMessage`, the desktop fulfils it via the
already-running LiteLLM proxy, the result returns over the same
channel, and a single `CallEvent` row records it.

## 2. Trust boundary recap

Same as `MCP-NATIVE.md` §4.4:

- Paired iPhone (Tailscale / Bonjour, device cert pinned).
- Local containers spawned by `BullmqRuntime` (Unix socket / VSOCK
  bind-mount; possession of the socket = capability).

No public exposure. The Ollama stack is bound to `127.0.0.1` and
LiteLLM's master key never leaves the host.

## 3. Protocol surface

### 3.1 Server-side advertise

When the desktop completes its MCP handshake with a peer, it
includes `sampling` in `serverCapabilities` (MCP standard):

```json
{
  "capabilities": {
    "sampling": {},
    "resources": { "subscribe": true },
    "tools":     { "listChanged": true },
    "prompts":   { "listChanged": true }
  }
}
```

(Note: in MCP, `sampling` is conventionally a *client* capability
because the host runs inference for connected servers. Here, the
desktop *is* the host for external servers, so it advertises the
capability outward. For the iPhone case the desktop is acting as a
*server* exposing tools, but it can still offer sampling — it's the
capability that matters, not the role.)

### 3.2 Inbound request

Standard MCP `sampling/createMessage`:

```jsonc
{
  "method": "sampling/createMessage",
  "params": {
    "messages": [
      { "role": "user", "content": { "type": "text", "text": "…" } }
    ],
    "modelPreferences": {
      "hints":             [{ "name": "qwen3.5" }, { "name": "claude-sonnet" }],
      "costPriority":      0.9,   // prefer cheap → local model first
      "speedPriority":     0.6,
      "intelligencePriority": 0.3
    },
    "systemPrompt": "…",
    "includeContext": "thisServer",
    "maxTokens": 2048,
    "stopSequences": ["</think>"],
    "temperature": 0.2
  }
}
```

### 3.3 Outbound response

```jsonc
{
  "result": {
    "role": "assistant",
    "content": { "type": "text", "text": "…" },
    "model":   "ollama/qwen3.5:35b-a3b-coding-nvfp4",
    "stopReason": "endTurn"     // endTurn | stopSequence | maxTokens
  }
}
```

Streaming uses MCP progress notifications (`notifications/progress`)
keyed by the request id; final completion arrives as the actual
response message.

## 4. Request lifecycle

```
agent (container)              desktop (host)               LiteLLM
─────────────────              ──────────────               ───────
sampling/createMessage  ─────▶ consent gate (§5)
                               model selection (§6)
                               record CallEvent.start
                               OpenAI-compat translate ────▶ /v1/chat/completions
                                                       ◀──── stream chunks
                       ◀──────  notifications/progress
                       ◀──────  notifications/progress
                                                       ◀──── final response
                               record CallEvent.end
                       ◀──────  result
```

Every step on the desktop side appends to the call stream the
inspector renders. Cancellation: peer sends
`notifications/cancelled` → desktop aborts the LiteLLM upstream
connection → final progress notification carries
`stopReason: "cancelled"`.

## 5. Consent gate

Per `MCP-NATIVE.md` §5:

| Peer | Default mode | Override |
|---|---|---|
| Paired iPhone | `allow-trusted` | manage in registry route |
| `BullmqRuntime` container | `allow-trusted` | per-job `samplingPolicy` field |
| Ad-hoc external server | `prompt` | `servers.toml` flip to `allow-implicit` |

`prompt` mode shows: *"Server `<name>` wants to ask Qwen3.5-35B for
2048 tokens — Allow once · Allow for session · Deny."* The decision
becomes a `CallEvent` of method `consent/grant` with `parent_id`
linking it to the sampling request.

`deny` returns:

```json
{ "error": { "code": -32001, "message": "sampling_denied" } }
```

(Reserved error code; we'll register it in our extensions table.)

## 6. Model selection

Pure function of `(modelPreferences, host_capabilities)`:

1. **Hint match.** Walk `hints[]` in order; the first hint whose
   `name` is a *prefix* of any configured `model_list` entry wins.
   `"qwen3.5"` matches `qwen3.5:35b-a3b-coding-nvfp4`.
2. **Priority weighting.** No hint match? Score each available model
   by `α·cost⁻¹ + β·speed + γ·intelligence` where (α, β, γ) come
   from the priorities. Local NVFP4 wins when `costPriority ≥ 0.7`.
3. **Hardware floor.** Skip qwen3.5-35b on hosts with < 32 GB
   unified memory (read once at startup; cached).
4. **Fallback.** If no model matches, return
   `{ error: "no_suitable_model" }` so the peer can fall back to its
   own provider.

Selection is deterministic and logged in
`CallEvent.params_summary` so the inspector can show *why* a given
model ran.

## 7. Parameter mapping (MCP → LiteLLM)

| MCP field | LiteLLM / OpenAI field | Notes |
|---|---|---|
| `messages[]` | `messages[]` | direct (role + content) |
| `systemPrompt` | prepended `{role:"system"}` | merged if `messages[0]` is also system |
| `maxTokens` | `max_tokens` | enforced by LiteLLM `request_timeout` too |
| `stopSequences[]` | `stop[]` | union with `default_litellm_params.stop` |
| `temperature` | `temperature` | passes through |
| `includeContext` | n/a | desktop attaches relevant resource snippets if `thisServer` or `allServers` |

`includeContext = "allServers"` means the desktop pulls live
resource state from every connected server it has subscriptions
into, summarises with a small local model, and prepends as a
system-level context block. This is the bit that makes
*sampling-with-context* useful — it's the desktop's superpower over
a vanilla Ollama call.

### 7.1 The summary model

Summarisation runs on a *separate* model from the primary sampling
call to avoid recursive blow-up (a 35B summarising for a 35B is
wasteful). Pinned choice:

- **`qwen3:4b`** (Qwen3 4B Instruct) — primary summary lane.
  Configured in `install/ollama-stack/litellm.config.yaml`.
  Same tokenizer family as the `qwen3.5:35b-a3b-coding-nvfp4`
  primary, so token accounting after concatenation is predictable.
  Fits in unified memory alongside the 35B on a 32 GB host.
- **`llama3.2:3b`** — fallback summary lane. Smaller, very
  well-optimised on M-series. Used when Qwen3-4B is unavailable or
  feels too verbose for terse-summary use cases.

Summary calls themselves go through LiteLLM and emit their own
`CallEvent` rows (with `parent_id` = the originating sampling
request), so the inspector can show the cost of `includeContext`
explicitly.

## 8. Container plumbing

`BullmqRuntime` (`crates/crabcc-cli/src/agent_bullmq.rs`) currently
needs `OLLAMA_HOST` (or a LiteLLM URL) injected into each container.
Replace with:

1. Mount a Unix socket from the desktop into the container at a
   well-known path (`/run/crabcc/mcp.sock`).
2. Drop the `OLLAMA_HOST` env var and any LiteLLM bearer.
3. Inside the container, the agent connects over the socket using
   the standard MCP stdio transport (socket → newline-delimited
   JSON-RPC).
4. Agent issues `sampling/createMessage` for inference and uses
   exposed `desktop.*` tools for memory / logs / spawning siblings.

For Apple `container` (issue #112) the equivalent is VSOCK on a
host-defined CID. Same protocol, different transport.

### 8.1 `BullmqRuntime` changes (concrete)

- Drop `env.OLLAMA_HOST`, `env.OLLAMA_API_KEY`, `env.LITELLM_*`.
- Add `--mount type=bind,source=/var/run/crabcc/mcp.sock,target=/run/crabcc/mcp.sock`.
- Pass `--network none` (or default-deny) — agent has zero direct
  network egress; any LLM call is mediated.
- Set `CRABCC_MCP_SOCKET=/run/crabcc/mcp.sock` so the agent runtime
  knows where to dial.

## 9. Audit & inspector view

Every sampling request emits two `CallEvent`s (start + end) plus N
progress events. Inspector special-cases sampling rows:

- Render the chosen model badge (color = local vs cloud).
- Show prompt token / completion token counts in the latency column.
- Show estimated cost (zero for local).
- "Open trace" button → opens the LiteLLM-side log line via
  `x-request-id` correlation header (LiteLLM already forwards it
  per `forward_client_headers_to_llm_api: true`).

## 10. Failure modes

| Failure | Returned error | Inspector renders |
|---|---|---|
| Ollama daemon down | `sampling_unavailable` (-32002) | red row, link to `ollama-stack` route |
| Model not pulled | `model_not_loaded` (-32003) | row with "Pull model" action |
| LiteLLM rate-limit | passes through `429` mapped to `rate_limited` (-32004) | yellow row, retry-after |
| Cancelled by client | `cancelled` (no error, just `stopReason`) | greyed row |
| Consent denied | `sampling_denied` (-32001) | row with the consent decision pinned |

All errors are typed so peers can branch on them.

## 11. Implementation cut

Smallest first commit that lands the loop end-to-end:

1. Add `sampling` capability to the desktop's MCP server handshake.
2. Wire a `SamplingHandler` trait + `LiteLlmSamplingHandler` impl
   that proxies to `http://127.0.0.1:4000/v1/chat/completions`.
3. Hardcode the consent gate to `allow-trusted`-for-localhost.
4. Skip `includeContext` (defer to v1.1).
5. Skip streaming (block until done; revisit once buffered demos
   feel slow).
6. Inspector: just render sampling as another `CallEvent` row — no
   special UI yet.

That's a one-PR scope. Streaming, context-injection, and the consent
toast UI are follow-ups.

## 12. Open questions & resolved decisions

### 12.1 Resolved

- **Sampling-of-sampling depth — hard cap at 3.** Each
  `sampling/createMessage` request carries a `_meta.samplingDepth`
  field (default 0); the desktop increments it before forwarding any
  nested sampling that the chosen model triggers via tool-use, and
  rejects with `-32005 sampling_depth_exceeded` when the incoming
  value is ≥ 3. A peer that *itself* recurses (its own model calls
  back into us) carries the depth header from its parent request,
  so the cap survives across the whole chain. Logged in
  `CallEvent.params_summary` as `depth=N`.
- **Summary lane — `qwen3:4b` (primary), `llama3.2:3b` (fallback).**
  See §7.1.

### 12.2 Deferred

- **iPhone transport — TBD, depends on Telegram bot.** The iPhone
  path is *not* currently a direct MCP-over-Tailscale link; it's
  routed through the in-progress Telegram-bot integration
  (host Docker bot ↔ Telegram chat ↔ iPhone). Telegram conversations
  don't stream — they edit messages incrementally — so the natural
  fit for that lane is "dispatch + poll with edit-based progress".
  Defer the question of whether the desktop *also* offers a direct
  iPhone-MCP transport until the Telegram bridge stabilises; the
  bridge may be sufficient on its own. When this is revisited, the
  protocol question is whether progress notifications survive the
  Telegram round-trip or need to be coalesced.
- **Cost accounting** — show running token cost per peer in the
  registry route? Probably yes; trivial once we record token counts.
- **Model swap mid-stream** — LiteLLM's fallbacks can swap us from
  Anthropic to Sonnet mid-flight. Surface that in the response's
  `model` field accurately, even if it differs from the chosen one.
