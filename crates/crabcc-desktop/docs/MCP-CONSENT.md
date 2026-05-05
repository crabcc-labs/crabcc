# MCP Consent — Identity, Trust, and Authorisation

> Companion to `MCP-NATIVE.md` (esp. §4.4 trust boundary and §5
> consent model), `MCP-SAMPLING-OFFER.md` (sampling-specific consent),
> and `MCP-INSPECTOR.md` (consent decisions are `CallEvent`s).
>
> Status: **draft**. Targets M4 of the roadmap in
> `MCP-NATIVE.md` §9.

## 1. Goal in one sentence

Make the desktop's "who can do what to me" decisions explicit,
revocable, and auditable, while staying out of the way for the
narrow trust set we actually have (paired iPhone, local containers).

## 2. Threat model

Given the trust boundary in `MCP-NATIVE.md` §4.4, we are **not**
defending against:

- An attacker on the open internet — there is no public listener.
- A compromised npm dependency in some random MCP server — those
  aren't installed; this isn't Cursor.
- A multi-user host — single-owner Mac.

We **are** defending against:

- A bug in our own agent code that fires unintended write tools
  (`agent.spawn` in a loop; `command.run` with a runaway argument).
- An external server we *did* install (slack, github, fff)
  doing something surprising during a release where it shouldn't.
- The owner's iPhone being momentarily in someone else's hand at a
  café — the screen-unlock has already happened, but we still want
  destructive actions to require a fresh nudge.
- A container escape from `BullmqRuntime` (low probability with
  Apple `container` / Docker isolation, but the consequence — full
  desktop control via the MCP socket — would be high).

Translation: consent here is **mostly about visibility, blast-radius
caps, and revocability**, not adversarial defence. The exception is
the container-escape case in §6.

## 3. Identity & pairing

How a peer proves *who it is* before any tool can fire:

| Peer kind | Identity proof | Where verified |
|---|---|---|
| Paired iPhone | TLS cert pinned during pairing; cert fingerprint stored in `~/.crabcc/desktop/peers.toml` | Tailscale or Bonjour transport |
| `BullmqRuntime` container | Possession of the bind-mounted Unix socket / VSOCK CID | Kernel-level — only PIDs inside our spawned container can `connect()` |
| Ad-hoc external server | Pre-shared token in `servers.toml`, or stdio (parent-process trust) | Per-connection handshake |
| In-proc crabcc server | Implicit (same process) | n/a |

### 3.1 iPhone pairing flow (one-time)

1. User triggers *Pair iPhone* in the desktop's registry route.
2. Desktop generates a one-time pairing nonce (32 bytes, 5-minute
   TTL) and renders it as a QR code.
3. iPhone client scans the QR, derives a fresh keypair, and POSTs
   `{ pubkey, device_name, nonce }` over Tailscale/Bonjour.
4. Desktop verifies the nonce, displays the device name and
   pubkey-fingerprint, asks for explicit confirmation.
5. On confirmation: writes `{ device_name, pubkey, paired_at }` to
   `~/.crabcc/desktop/peers.toml` and treats the device as
   `allow-trusted`.
6. Subsequent connections require the device to prove possession of
   the private key (TLS client cert or signed challenge).

Unpairing is a one-click action in the registry route; removes the
peer entry, terminates any live session.

## 4. The consent matrix

Per `(peer × tool)` pair, exactly one of:

- **`allow-trusted`** — fires without prompting, recorded as a
  `CallEvent` with `consent.implicit = true`. Default for paired
  iPhone and `BullmqRuntime` containers.
- **`prompt`** — toast: *Allow once · Allow for session · Deny*.
  Default for ad-hoc external servers in their first session.
- **`allow-implicit`** — declared in `servers.toml` (per-server,
  per-tool) for stable trusted servers (e.g. `fff` after the user
  has approved its tools repeatedly).
- **`deny`** — never invokable; returns
  `-32001 sampling_denied` (or the tool-call equivalent).

The `(peer × tool)` granularity matters: the iPhone can be
allow-trusted for `desktop.memory.search` but `prompt` for
`desktop.command.run`. See §5 for the sensitive-tool taxonomy that
overrides defaults.

## 5. Sensitive-tool taxonomy

Some tools **always** prompt — even for `allow-trusted` peers —
because their blast radius is too large to bake into a one-time
pairing decision. The list is small and explicit:

| Tool | Reason |
|---|---|
| `desktop.command.run` | Arbitrary subprocess; argument-injection risk even on trusted peers |
| `desktop.window.focus` & friends, when *focus-stealing* | Trivial nuisance vector; cheap to prompt |
| `desktop.agent.spawn` with `prompt_size > 32 KB` | Runaway-cost guard, cap is configurable |
| `sampling/createMessage` with `maxTokens > 4096` | Per-call cost cap, configurable |
| `desktop.memory.delete` / `desktop.memory.forget` | Destructive, irreversible at the user-data level |

Per-tool overrides live in `servers.toml`:

```toml
[server.iphone.tools."desktop.command.run"]
mode = "allow-trusted"   # explicit override; user knows what they're doing
```

The override requires the user to type it manually — there's no UI
for downgrading a sensitive tool to allow-trusted. That deliberate
friction is the whole point.

## 6. The container-escape exception

This is the one place the trust model *isn't* "vibes-based":

A `BullmqRuntime` container holds the MCP socket and is by default
`allow-trusted`. If a container escape happens, the attacker has
full read/write to the desktop's tool surface. Mitigations:

1. **`--network none`** on every container by default (see
   `MCP-SAMPLING-OFFER.md` §8.1). The escape can't reach the
   internet directly.
2. **Sensitive-tool taxonomy still applies** — even an escaped
   container can't run `command.run` without prompting.
3. **Per-job consent scope** — when `BullmqRuntime` enqueues a job,
   it carries a `consentScope` field listing the tools the job is
   *expected* to need. Anything outside that list prompts even
   though the peer is allow-trusted.
4. **Idle timeout** — sockets that go silent for > 5 min are
   tear-down candidates. Long jobs send heartbeats; lost heartbeat
   = closed socket = lost capability.

## 7. Storage

All consent state lives in three places:

| File | Contents | Lifecycle |
|---|---|---|
| `~/.crabcc/desktop/peers.toml` | Paired devices (pubkey, name, paired_at) | Edited via UI, manual edits supported |
| `~/.crabcc/desktop/servers.toml` | Per-server tool-mode overrides | Hot-reloaded on save |
| `.crabcc/desktop/consent.db` (SQLite) | Per-session grants ("allow for session" decisions) | Cleared on app restart; survives sleep |

Schema is additive (per the crabcc convention):

```sql
CREATE TABLE session_grants (
  id          BLOB PRIMARY KEY,    -- ulid
  ts          INTEGER NOT NULL,
  peer        TEXT    NOT NULL,
  tool        TEXT    NOT NULL,
  scope       TEXT    NOT NULL,    -- 'once' | 'session'
  granted_by  TEXT    NOT NULL,    -- 'user' | 'rule:<name>'
  expires_at  INTEGER              -- NULL = end-of-session
);
CREATE INDEX idx_grants_peer_tool ON session_grants(peer, tool);
```

## 8. The toast UI

When `prompt` mode fires:

```
┌─────────────────────────────────────────────────────────────┐
│  Slack wants to send a message to #engineering              │
│  ─────────────────────────────────────────────────────────  │
│  tool:    slack.send_message                                │
│  args:    { "channel": "#engineering",                      │
│             "text":    "Build is green!" }                  │
│  blast:   slack://channels/engineering                      │
│                                                             │
│  [ Allow once ]  [ Allow for session ]  [ Deny ]            │
└─────────────────────────────────────────────────────────────┘
```

Auto-dismisses after 30 s as *deny*. Decision becomes a `CallEvent`
with method `consent/grant` (or `consent/deny`), `parent_id` linking
it to the pending tool call. The pending call hangs until decided —
not retried, not failed-silently. Peer sees a typed error or the
delayed result.

For sensitive tools (§5), the toast adds a red *Sensitive tool*
banner and a *Blast radius* line summarising what state will change.

### 8.1 What gets rendered

- The full args, but with values matching the secret-shape regexes
  (§9 of `MCP-INSPECTOR.md`) replaced by `[REDACTED]`. The user can
  click *show secrets* to expand — the prompt itself is on a trusted
  surface (the user's screen), so this is a paste-into-Slack
  guard, not a confidentiality boundary.
- A history line: *"Last time you saw this prompt: 2 hours ago →
  allowed for session"*. Lets the user spot anomalies.

## 9. Revocation

Three paths:

1. **Long-press a `consent/grant` row in the inspector.** Revokes
   the session grant immediately; future calls re-prompt.
2. **Registry route → `Active grants` panel.** Tabular view of
   live session grants. *Revoke all* button.
3. **Kill switch.** `desktop.consent.revoke_all` is itself an MCP
   tool (callable from the iPhone). Always `prompt`. Useful when
   you realise something's off and you're not at the keyboard.

Revocation is **immediate**. Any in-flight tool call holding a
grant on a since-revoked tuple finishes if it was already past the
gate; new calls re-prompt.

## 10. The audit trail

Per `MCP-INSPECTOR.md` §2, every consent decision (grant, deny,
revoke) is a `CallEvent`:

- `method = "consent/grant" | "consent/deny" | "consent/revoke"`
- `parent_id` → the tool-call that triggered the gate (for grants
  and denials), or the prior grant (for revokes).
- `params_ref` → snapshot of the toast contents.
- `result_ref` → the user's choice + scope.

The inspector route gets a *Consent decisions* preset filter; same
data, focused view. Long-press → revoke.

## 11. Pairing edge cases

- **Lost iPhone.** Unpair from the registry route on the desktop;
  the device's pubkey is invalidated. If the desktop itself is
  unreachable and the iPhone is lost: the device can't act because
  containers and external servers don't grant it any ambient
  authority — the iPhone's authority is purely keyed on its pubkey,
  and re-pairing requires physical access to the desktop.
- **Multiple iPhones.** Allowed; each gets a separate row in
  `peers.toml`. Useful for "old iPhone before sale" cleanup.
- **Pairing while already paired.** Treated as a *re-pair* — old
  pubkey is invalidated and replaced. UI confirms with a banner.

## 12. Implementation cut

Smallest first commit that lands the model:

1. `peers.toml` + `servers.toml` schema and parsers; in-memory
   resolution function `(peer, tool) → Mode`.
2. `consent.db` with `session_grants` and the four scope values.
3. Toast UI — gpui modal with the three buttons; renders args via
   the same JSON viewer as the inspector.
4. Sensitive-tool taxonomy (§5) hardcoded in v1; configurable later.
5. `consent/grant`-style `CallEvent`s emitted into the same
   broadcast channel as everything else.

Defer to v1.1+: iPhone pairing flow (depends on the Tailscale/Bonjour
work), `desktop.consent.revoke_all` MCP kill switch, the
*Active grants* panel UI.

## 13. Open questions

- **Heartbeat interval** — §6 says 5 min. Too aggressive for
  long-running agent jobs that legitimately think for 10 min? Maybe
  expose as a per-job override.
- **Auto-dismiss action** — §8 says auto-dismiss = deny after 30 s.
  An iPhone toast *always* dismissing as deny while the user is
  AFK is conservative-but-noisy. Consider a "let me decide later"
  scope that parks the request in a queue you can flush manually.
- **`servers.toml` plaintext bearer** — fine for the trust boundary
  we have, but if a future version pulls from environment with no
  fallback, document the env-var name (`CRABCC_DESKTOP_*`) and the
  shell-history risk.
- **Per-room consent in a future memory-palace UI** — the MemPalace
  has wing/room scoping; once those land in the desktop, consent
  may want a `room` axis. Out of scope today.
