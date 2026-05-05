# MCP Inspector — Wire Format & Storage Spec

> Companion to `MCP-NATIVE.md` (esp. §2 "The Inspector") and
> `MCP-SAMPLING-OFFER.md`. Commits to the concrete shape of the
> `CallEvent` record, the on-disk storage layout, the in-process
> broadcast channel that drives the inspector route, and the
> `desktop://mcp/calls` MCP resource that publishes the same stream
> to connected peers.
>
> Status: **draft**. Targets M0 of the roadmap in
> `MCP-NATIVE.md` §9.

## 1. Goal in one sentence

Every MCP message — internal or external, in or out, success or
error — becomes one durable, content-addressed `CallEvent` row that
the inspector can render at 60 fps, replay safely, and publish to
external subscribers.

## 2. The `CallEvent` record

```rust
use compact_str::CompactString;
use time::OffsetDateTime;
use ulid::Ulid;

pub struct CallEvent {
    /// Monotonic, sortable, time-prefixed id.
    pub id:           Ulid,
    pub ts:           OffsetDateTime,

    /// Which side of the wire this came from.
    pub server:       ServerId,        // SmolStr-like
    pub agent_origin: AgentId,         // "self" for internal traffic
    pub transport:    Transport,       // Stdio | Unix | Vsock | InProc | Tailscale
    pub direction:    Direction,       // In | Out

    /// MCP method name + extracted tool name (when method = "tools/call").
    pub method:       CompactString,
    pub tool_name:    Option<CompactString>,

    /// Content-addressed pointers into the payload store. Lookups
    /// are lazy — the row alone fits in a 256-byte ring slot.
    pub params_ref:   PayloadRef,         // BLAKE3-32
    pub result_ref:   Option<PayloadRef>,

    pub status:       Status,             // Pending | Ok | Err { code, msg }
    pub latency_ms:   Option<u32>,

    /// Causality. Every nested call carries the parent's id.
    pub parent_id:    Option<Ulid>,
    /// If this event is a replay, points at the original.
    pub replay_of:    Option<Ulid>,
    /// For sampling: depth in the recursion chain (cap = 3, see
    /// MCP-SAMPLING-OFFER.md §12.1).
    pub sampling_depth: Option<u8>,

    /// Resource URIs whose state this call invalidated. Drives
    /// re-fetch in subscribed peers and the "blast radius" badge in
    /// the inspector's right pane.
    pub invalidated:  SmallVec<[ResourceUri; 2]>,

    /// Sampling-specific telemetry (None for non-sampling rows).
    pub sampling:     Option<SamplingTelemetry>,

    /// Consent decision attached to this call (None when no gate
    /// fired — i.e. allow-trusted peers).
    pub consent:      Option<ConsentRef>,
}

pub struct SamplingTelemetry {
    pub model:             CompactString,    // resolved model
    pub model_requested:   Option<CompactString>, // hint, if any
    pub prompt_tokens:     Option<u32>,
    pub completion_tokens: Option<u32>,
    pub finish_reason:     Option<FinishReason>,
}
```

Sized small. The ring-buffer slot for one event is ≤ 256 B because
all the variable-length data lives behind `PayloadRef`s.

## 3. Storage tiers

```
                        ┌──────────────────────────┐
   broadcast channel    │  ring buffer (10 k rows) │  ←  inspector renders from here
   (postcard frames) ──▶│  in-memory, RwLock-free  │
                        └────────────┬─────────────┘
                                     │ append (background task)
                                     ▼
                        ┌──────────────────────────┐
                        │  calls.db (sqlite WAL)   │  ←  durable, 7-day retention
                        │  events table + indexes  │
                        └────────────┬─────────────┘
                                     │ payload puts (BLAKE3)
                                     ▼
                        ┌──────────────────────────┐
                        │  payloads/  (CAS store)  │  ←  zstd-frame on disk
                        │  one file per BLAKE3     │     dedupes repeat args
                        └──────────────────────────┘
```

### 3.1 Ring buffer

- Fixed capacity, lock-free SPMC (`tokio::sync::broadcast` over the
  `CallEvent` value-type, not the payloads).
- Inspector subscribes once on mount. Lagging is expected (UI thread
  drops frames) and tolerated — the durable log is the source of
  truth for "load older".

### 3.2 `calls.db` (SQLite, WAL)

`.crabcc/desktop/calls.db`. Schema is **additive only** (per the
crabcc convention in `CLAUDE.md`):

```sql
CREATE TABLE events (
  id              BLOB PRIMARY KEY,        -- ulid bytes
  ts              INTEGER NOT NULL,        -- unix-millis
  server          TEXT    NOT NULL,
  agent_origin    TEXT    NOT NULL,
  transport       TEXT    NOT NULL,
  direction       INTEGER NOT NULL,        -- 0=In, 1=Out
  method          TEXT    NOT NULL,
  tool_name       TEXT,
  params_ref      BLOB    NOT NULL,        -- 32-byte BLAKE3
  result_ref      BLOB,
  status_code     INTEGER NOT NULL,        -- 0=Pending,1=Ok,2=Err
  status_err_code INTEGER,
  status_err_msg  TEXT,
  latency_ms      INTEGER,
  parent_id       BLOB,
  replay_of       BLOB,
  sampling_depth  INTEGER,
  invalidated     TEXT,                    -- JSON array
  consent_id      BLOB,
  -- sampling telemetry (NULL for non-sampling rows)
  s_model            TEXT,
  s_model_requested  TEXT,
  s_prompt_tokens    INTEGER,
  s_completion_tokens INTEGER,
  s_finish_reason    TEXT
);

CREATE INDEX idx_events_ts            ON events(ts DESC);
CREATE INDEX idx_events_server_ts     ON events(server, ts DESC);
CREATE INDEX idx_events_parent_id     ON events(parent_id);
CREATE INDEX idx_events_replay_of     ON events(replay_of);
CREATE INDEX idx_events_method_status ON events(method, status_code);
```

Retention default: 7 days, GC'd every hour by a background task
(`DELETE WHERE ts < now() - 7d` + `VACUUM` weekly).

### 3.3 Content-addressed payload store

`.crabcc/desktop/payloads/<BLAKE3-hex>.zst`. One file per blob,
zstd-frame compressed at level 3. Identical params/results dedupe to
one file. GC runs after the events GC: any payload not referenced by
a surviving row is deleted.

Why not stuff payloads in the events table? Two reasons:
- An MCP message can be megabytes (a `resources/read` of a large
  file); a 10 k-row sqlite with megabyte BLOBs is awful to scan.
- Content addressing means a slack notification fired 100 times
  costs one payload, not 100.

## 4. The in-process broadcast

The renderer↔core internal bridge already speaks MCP shape (see
`MCP-NATIVE.md` §4.1). Every message that flows through it is also
forwarded to a `tokio::sync::broadcast::Sender<CallEvent>`. The
ring-buffer subscriber and the durable-log subscriber both receive
from this fan-out. External MCP traffic goes through a thin adapter
that constructs `CallEvent`s identically.

Encoding on the channel: the `CallEvent` struct directly (no
serialization). Encoding on the durable side: the column shape above.
Encoding on the wire (when published over `desktop://mcp/calls`):
NDJSON, `serde_json` of `CallEvent` with payload-refs as hex BLAKE3s.

## 5. The `desktop://mcp/calls` resource

Subscribable MCP resource. Three operations:

| Operation | MCP method | Behaviour |
|---|---|---|
| Snapshot | `resources/read` | Returns the most-recent N events as NDJSON; `?since=<ulid>` for tailing. |
| Subscribe | `resources/subscribe` | Future events stream as `notifications/resources/updated`. |
| Payload fetch | `resources/read` of `desktop://mcp/payload/<blake3>` | Returns one payload blob. |

**Privacy & redaction.** Before any event leaves the host via this
resource, a redaction pass runs (§9). Internal in-process subscribers
see the unredacted form.

## 6. Filtering & query model

The inspector route maintains a `FilterSpec`:

```rust
pub struct FilterSpec {
    server:       Option<ServerId>,
    agent_origin: Option<AgentId>,
    method_glob:  Option<CompactString>,    // "tools/*", "sampling/*"
    status:       Option<StatusKind>,        // Pending | Ok | Err
    since:        Option<OffsetDateTime>,
    parent_root:  Option<Ulid>,              // show only this causality tree
    sampling:     Option<bool>,              // sampling rows only
    text:         Option<CompactString>,     // substring across method/tool/server
}
```

Live mode filters the broadcast in-process; "load older" issues a
prepared SQLite SELECT with the same predicates against the indexes.
Each filter slot has a covering index (§3.2).

## 7. Diff algorithm

When the user selects two rows and clicks *Diff*:

1. Fetch both `params_ref`s (and optionally `result_ref`s).
2. Run `serde_json::Value::diff` (we'll use the
   [`json-patch`](https://crates.io/crates/json-patch) crate — RFC
   6902 patches, well-supported).
3. Render the patch as a structural side-by-side: additions green,
   removals red, scalar changes inline.

Diffs are computed lazily and cached by `(left_ref, right_ref)`.

## 8. Replay

### 8.1 Read-replay (idempotent)

For methods marked `idempotent` in the registry (e.g.
`resources/read`, `tools/call` of read-only tools, `sampling/*` if
temperature = 0):

- Re-issue the request with the original params.
- Diff the new result against the recorded result.
- Render a "drift" badge on the row if they differ.

### 8.2 Write-replay (side-effecting)

- Gated by a fresh consent toast (even for `allow-trusted` peers —
  replays are user-initiated).
- The new event carries `replay_of: <original_id>`.
- The inspector renders it nested under the original.
- Tools marked `non_deterministic = true` in the registry (e.g.
  `desktop.agent.spawn`) render the divergence badge as **expected**
  rather than red — the user is told "this is supposed to differ".

### 8.3 Replay of nested chains

Replay is single-event by default. *Tree replay* (re-fire the entire
sub-tree under a parent) is a follow-up; needs deterministic ordering
and a way to handle child-event id rewrites. Out of scope for v1.

## 9. Redaction pass

Before publishing to `desktop://mcp/calls`:

- Strip values matching the secret-shape regexes (Stripe keys, JWTs,
  bearer tokens, AWS access keys). Same set crabcc already uses for
  log scrubbing — reuse, don't reinvent.
- Replace the payload-ref with a redacted variant (different BLAKE3),
  so peers can't reach the original via direct payload fetch.
- Honour per-server `redact_params` / `redact_result` overrides in
  `servers.toml` for stricter peers.

In-process consumers (the inspector route itself, internal logging)
see the unredacted form. The rationale: the user *should* be able to
see secrets they own when debugging on their own machine; only
network-exposed surfaces strip them.

## 10. Performance budget

- **Render budget**: 16 ms / frame at 60 fps with 10 k rows visible
  via virtualisation. No row should fault-in payload data — the row
  is only the columns listed in §3.2 minus `params_ref`.
- **Append budget**: ≤ 5 µs to push a new event onto the broadcast
  channel. Anything heavier (sqlite insert, payload write) runs on a
  background task; the hot caller is never blocked.
- **Sustained throughput**: 1000 events/sec without dropped frames
  on the inspector. Slack-style chat agents with N×N message fan-out
  can hit that, so it's a real budget, not a stretch goal.
- **Memory ceiling**: ~3 MB for the in-memory ring (10 k × ~256 B);
  plus whatever the visible payloads pin (we evict them from the
  payload-cache LRU on inspector close).

## 11. Inspector UI affordances

Per `MCP-NATIVE.md` §2 — fleshed out:

### Left pane (timeline)

Columns, in display order:

- Time (relative, "12 ms ago")
- Server (color chip + short id)
- Direction arrow (← In, → Out)
- Method (faded if it's the boring half — `notifications/*`,
  `ping`)
- Tool / model badge (for `tools/call` → tool name; for
  `sampling/createMessage` → model name)
- Latency bar (log-scale; > 1 s is red)
- Status icon (✓ / ⏳ / ⚠)
- Depth badge (sampling chains; > 0 only)
- Replay badge (when `replay_of.is_some()`)

Filter bar across the top maps 1:1 to `FilterSpec` (§6).

### Right pane (detail)

Tabs: **Params · Result · Diff · Causality**.

- *Params / Result* — JSON viewer with collapsible nodes. Bytes
  badge at the top (raw size + compressed size, so you can see how
  much the CAS store earned).
- *Diff* — only enabled when two rows are selected (or when this is
  a read-replay event).
- *Causality* — tree view rooted at `parent_id` ancestor. Click a
  node to jump to its row.

### Right-pane footer (always visible)

- *Pin* — adds the event id to `scenarios.toml`.
- *Replay* — see §8.
- *Open trace* — for sampling rows, jumps to the LiteLLM
  `x-request-id` correlated log line.

## 12. Pinned scenarios → tests

`scenarios.toml` is a list of pinned event ids:

```toml
[[scenario]]
id          = "01H8Z…"
name        = "agent-spawn-with-empty-prompt"
note        = "regressed once when memory drawer was unset"
created     = 2026-05-05T17:31:00Z
```

`crabcc desktop scenarios export <name>` rewrites it into a
`tests/scenarios/<name>.rs` integration test that replays the event
and asserts on the recorded result. The replay engine here is the
same one used by the inspector's *Replay* button — single
implementation, two consumers.

## 13. Implementation cut

Smallest first commit:

1. `CallEvent` + the broadcast channel in the in-proc bridge —
   internal traffic only.
2. Ring buffer + naive inspector route reading from it (no SQLite,
   no CAS yet). Drop everything older than the ring's capacity.
3. Filter UI for server + status (deferred: glob, parent_root).
4. JSON-viewer right pane.

That's M0 in `MCP-NATIVE.md` §9. SQLite + CAS, redaction, replay,
external publication via `desktop://mcp/calls` are M1+ follow-ups,
each in its own PR.

## 14. Open questions

- **Payload store on Linux** — content addressing assumes a
  reasonable filesystem (APFS, ext4). Tested on macOS only at v1; a
  later pass for Linux may want to fall back to a single sqlite
  blob-table if APFS-style hardlinks aren't available.
- **Privacy of the replay prompt** — when we toast "X wants to
  replay sampling/createMessage", do we render the prompt content
  inline, or show it only after the user expands? Current lean:
  collapsed by default; secrets-in-prompts are common.
- **Ring-vs-disk consistency window** — there's a brief moment
  where an event is in the ring but not yet in `calls.db`. If the
  app crashes there, the event is lost. Acceptable for v1 (the
  inspector is a debugger, not a SLA log) — call it out so a future
  reader doesn't expect durability of the *last* event.
