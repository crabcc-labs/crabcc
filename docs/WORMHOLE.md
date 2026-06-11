# WORMHOLE: Spec + Plan

Operator control channel for crabcc. Secure (E2EE through untrusted relay),
recoverable (sessions survive disconnects, roaming, crashes), resilient (4G,
NAT, many nodes). MVP path from the research report: Happy-shaped dumb relay +
SPAKE2 pairing + Noise_IK + offset replay. iroh deferred to v2.

## 1. Goals / Non-goals

Goals:
- One operator (dashboard or CLI) controls many crabcc agent sessions across the fleet.
- Works off-Tailscale: operator on hostile 4G reaches NAT'd nodes via a public Hetzner relay.
- Relay is blind: ciphertext, routing metadata, hashed identities only.
- One-time human-transcribed pairing code per node; never again.
- Logical session survives socket death, IP change, operator client restart, relay restart.

Non-goals (explicit):
- No transport-layer QUIC migration (v2 / iroh).
- No multi-operator concurrency, no CRDTs.
- No hole-punching / direct paths off-Tailscale. On-net, Tailscale already gives direct; off-net, relay only.
- No file transfer (control + event stream only; cap message size).

## 2. Topology

```
operator (browser dashboard / crabcc CLI)
        |  wss:// (outbound 443)
        v
  wormhole-relay (public Hetzner box)        <- blind forwarder + encrypted blob log
        ^
        |  wss:// (outbound 443, persistent)
crabcc node daemon (N nodes: hetzner, dev-cx53, mbp, pi)
```

Fast path: when operator and node share the tailnet, operator connects to the
node's wormhole listener directly over Tailscale (same protocol, no relay, no
blob log latency). Relay is the fallback, selected automatically by reachability
probe.

## 3. Identity and keys

| Key | Holder | Use |
|---|---|---|
| `op_root` Ed25519 | operator (M3 Pro) | biscuit root, signs auth challenges |
| `op_static` X25519 | operator | Noise IK initiator static |
| `node_static` X25519 + Ed25519 | each node | Noise IK responder static; signs presence |
| session ephemerals | both | per-connection, Noise `ee`, forward secrecy |

Relay stores `node_id = BLAKE3(node_static_pub)` only. Plaintext pubkeys never
persist on the relay (Happy pattern).

All long-term private keys flow through sops-nix (`secrets/wormhole.yaml`, age
recipients derived from host SSH keys via ssh-to-age, decrypted to
`/run/secrets/wormhole/*`, `restartUnits = [ "crabcc-wormhole.service" ]`).
Pairing codes are ephemeral, never committed.

## 4. Pairing protocol (one-time per node)

Decision (surfaced, default chosen): run SPAKE2 **through the wormhole-relay
itself** rather than depending on the magic-wormhole mailbox server. One server
instead of two; the relay already provides rendezvous. Alternative:
`magic-wormhole` crate + self-hosted mailbox; rejected for MVP as a second
moving part.

Flow:
1. Node: `crabcc wormhole pair` generates code `<nameplate>-<word>-<word>-<word>`
   (3-word PGP wordlist, ~34 bits entropy), opens relay channel `pair/<nameplate>`
   at `/wormhole/v1/pair/<nameplate>`.
2. **Both sides immediately send `PairingHello { version: 1, role }`** over the
   plaintext WS. If versions differ the higher-version side aborts with
   `PairingError::VersionMismatch` — no downgrade. This frame is not encrypted;
   it exists only for protocol negotiation (magic-wormhole-inspired).
3. Both run SPAKE2 (`spake2` 0.4, Ed25519 group) over the relay channel; the
   3-word code is the password. One active MITM guess max; nameplate burned on
   first use, 120s TTL.
   Relay-side dilatory defence: on `PairingError::MacVerificationFailed` the
   relay sleeps a random 100–300 ms before responding so timing cannot reveal
   whether the nameplate exists. (magic-wormhole-inspired)
3. Inside the SPAKE2-derived channel: exchange `op_static_pub`, `op_root_pub`,
   `node_static_pub`, `node_ed_pub`, plus a transcript MAC binding the PAKE key
   to the exchanged statics (channel binding; RustCrypto warning).
   MAC input MUST cover the full ordered transcript:
   `BLAKE3(PAKE_key || sender_pub || receiver_pub || nameplate)`. A partial MAC
   (e.g. only one static) allows substitution of the other; failure must abort and
   burn the nameplate — no retry with the same code (security residual R1-F1).
   Use 3-word PGP-wordlist codes (~34 bits entropy) not 2-word (~17 bits).
4. Both persist the peer statics. Operator mints the node's first biscuit. Done;
   code is dead.

## 5. Channel protocol

- Outer transport: WebSocket over TLS (tokio-tungstenite on nodes/relay; native
  WS in browser).
- Inner: Noise_IK_25519_ChaChaPoly_BLAKE2s via `snow`. Node = responder
  (operator already knows `node_static_pub` from pairing). Dashboard runs `snow`
  compiled to WASM.
  - Surfaced tradeoff: snow-wasm keeps one audited-ish codepath both ends but
    adds a WASM build step. Alternative: libsodium crypto_box both ends
    (Happy-style, simpler in browser, hand-assembled handshake). Default:
    snow-wasm. Flip only if WASM integration into Dashboard v2.html is painful;
    say so before flipping.
- WS path: `wss://relay.crabcc.app/wormhole/v1` (nodes + operator); pairing at
  `/wormhole/v1/pair/<nameplate>`. The path prefix disambiguates from other
  services on the same host and makes future versioning explicit.
- Two-layer framing (R2-F1: structural relay/Noise separation):
  - **Outer** WS message: `OuterFrame { node_id: [u8;32], channel: u16, noise_payload: Vec<u8> }`.
    The relay deserialises only this type. `noise_payload` is forwarded verbatim; the relay
    never calls `Envelope::decode`.
  - **Inner** Noise transport payload: `Envelope`, postcard-serialized.

```rust
struct OuterFrame {
    node_id: [u8; 32],       // BLAKE3(node_static_pub) — routing key only
    channel: u16,            // 0 = control, 1+ = per-session
    noise_payload: Vec<u8>,  // opaque to relay
}

struct Envelope {
    session: SessionId,      // u128, OsRng; NEVER timestamps/counters
    seq: u64,                // per-session monotonic, sender-assigned
    kind: Kind,              // Hello, Resume{from_seq}, Cmd, Event, Ack{seq}, Ping, Pong, ...
    body: Vec<u8>,           // max MAX_BODY_BYTES (64 KiB); relay drops oversized frames
}
```

- Replay protection: Noise nonces per connection + `seq` checked monotonic per
  session across reconnects.
- Heartbeat: Ping every 20s, dead after 2 misses (mosh-style; also detects roam).

## 6. Recoverability

Two layers, independent:
1. Process persistence: agent sessions already run under Godfather. Wormhole
   disconnect never touches the session process. Node daemon buffers events while
   no operator is attached.
2. Resume protocol: relay keeps a per-(node_id, session) append-only log of
   **encrypted** event blobs with offsets, capped (default 64 MiB or 24h,
   whichever first; truncation surfaced to operator as `GapNotice{from,to}`). On
   reconnect, operator sends `Resume{from_seq}`; relay replays from offset, node
   fills anything past the relay's tail. Direct-Tailscale connections skip the
   relay log; the node's own buffer serves resume.
3. High-volume terminal output: do NOT replay byte streams. mosh SSP idea: node
   maintains numbered terminal snapshots; resume sends latest snapshot + tail,
   skipping intermediate frames. Cmd/control messages are always fully sequenced
   and acked, never skipped.

Not in MVP: 0-RTT anything. Reconnect pays the full Noise handshake (1 RTT).
Never put commands in any future early-data path.

## 7. Many nodes

- Relay routes by `node_id` in a cleartext outer frame header `{node_id,
  channel}`; payload opaque.
- Operator holds one wss connection to the relay, logical streams per
  node/session multiplexed by Envelope. No yamux in MVP (envelope routing
  suffices; revisit if head-of-line blocking bites on bulk output).
- Presence: relay tracks `{node_id, connected, last_seen}`; pushed to operator
  as `Presence` frames. This is the only plaintext-ish metadata the relay
  handles, and it is inherent to routing.

## 8. Authorization

biscuit (`biscuit-auth` v6): operator root key mints per-node tokens with
Datalog facts `node("…")`, `right("session:spawn")`, `right("session:attach")`,
TTL **1h** (reduced from 24h; automated refresh; limits blast radius to 1h on
token compromise — R1-F3). Node verifies offline with `op_root_pub` only.
Revocation: node-side revocation-ID list pushed over the channel; the list MUST
be signed by `op_root_ed25519` so it can be delivered out-of-band and verified
without trusting the channel (R1-F3b). Flag: biscuit has no published
third-party audit; acceptable for single-operator personal fleet.

`op_root` is a root CA equivalent — store offline (hardware key or encrypted
cold storage), never in process memory except during mint. If compromised,
recovery requires re-pairing all nodes. Key rotation of `node_identity_key`
(the X25519/Ed25519 static used for Noise IK and BLAKE3 hashing) MUST be a
deliberate pairing ceremony, not swept up in a general sops secret rotation
(R2-F4). Keep `node_identity_key` in its own sops entry without
`restartUnits`; separate it from rotatable auth secrets.

The relay does NOT verify biscuits (it stays dumb). Relay access control:
connecting party must sign a relay-issued nonce with a key whose BLAKE3 hash the
relay knows (registered at pairing time, operator key hash baked into relay
config). Keeps freeloaders off without the relay learning identities beyond
hashes.

## 9. Crates / pins

| Crate | Use | Status flag |
|---|---|---|
| `snow` | Noise_IK | mature, pure-Rust path unaudited; pin |
| `spake2` 0.4 | pairing | explicitly unaudited (their words); single-guess online-only exposure |
| `tokio-tungstenite` | WS node+relay | mature |
| `biscuit-auth` 6.x | capabilities | no formal audit |
| `blake3`, `ed25519-dalek`, `x25519-dalek` | identity | standard |
| `postcard` | envelope | standard |
| `axum` | relay HTTP/WS server | standard |

RUSTSEC monitoring via existing CI. Do not hand-roll any key exchange; everything
derives from snow or spake2 transcripts.

## 10. Repo layout (in crabcc)

```
crates/wormhole-proto/    envelope, session types, seq logic, no I/O
crates/wormhole-relay/    axum binary, blob log (redb or flat segment files: decide W1)
crates/wormhole-node/     daemon side, integrates Godfather session registry
crates/wormhole-op/       operator lib, compiles native + wasm32-unknown-unknown
nix: modules/wormhole-relay.nix, modules/wormhole-node.nix (peterlodri.wormhole.*), secrets/wormhole.yaml
```

## 11. Plan: 4 waves, 11 tasks

Wave 1: protocol + relay
1. `wormhole-proto`: Envelope, SessionId, seq/resume state machine. Verify:
   property tests, resume from arbitrary offset reproduces exact stream.
2. `wormhole-relay`: WS accept, nonce auth, route by node_id, append-only blob
   log + offset replay, caps + GapNotice. Verify: VB-W1 below.
3. Relay NixOS module + sops wiring. Verify: `nix flake check`; `nixos-rebuild
   build` for relay host.

Wave 2: pairing + node (needs W1)
4. SPAKE2 pairing over relay channel, channel binding MAC, key persistence.
   Verify: two local processes pair via local relay; wrong code fails closed;
   nameplate single-use enforced.
5. `wormhole-node` daemon: Noise responder, session registry against Godfather,
   event buffering, Cmd dispatch. Verify: spawn/attach a real crabcc session
   through local relay.
6. Node NixOS module + secrets. Verify: build on hetzner + dev-cx53 configs.

Wave 3: operator (needs W2)
7. `wormhole-op` native: CLI attach/spawn/list, resume on reconnect. Verify:
   kill -9 the CLI mid-stream, restart, zero lost Cmd/Event seqs.
8. `wormhole-op` wasm + Dashboard v2 integration: pair UI, node list with
   presence, session terminal panes. Verify: browser pairs and attaches
   end-to-end via relay.
9. Tailscale fast path: reachability probe, direct connect, relay fallback.
   Verify: same session reachable both paths; path switch mid-session resumes
   within one heartbeat interval.

Wave 4: hardening (needs W3)
10. biscuit mint/verify/revoke, TTL refresh over channel. Verify: expired/revoked
    token rejects Cmd, attach-only token cannot spawn.
11. Adversarial pass: relay-compromise drill (assert log decrypts to nothing
    useful), replay injection test, roam test on real 4G (phone hotspot flap),
    seq-gap fuzzing. Verify: VB-W4.

Verification blocks (repo-style):

```
VB-W1: relay blindness
  grep -R "decrypt\|snow::" crates/wormhole-relay/src   # pass: no output
VB-W2: pairing single-use
  integration test: second claim of same nameplate -> error, exit 0 on test pass
VB-W3: resume integrity
  test: drop socket at random seq x100 runs, replayed stream byte-identical
VB-W4: roam survival
  manual: wifi->4G mid-session, session continues, GapNotice only if relay cap hit
```

## 12. Open decisions (resolve before the wave that needs them)

1. Relay log store: redb vs flat segment files (W1). Lean: flat segments, simpler,
   log is append-only and capped.
2. snow-wasm vs libsodium-in-browser (W3, flip-trigger defined in §5).
3. Relay host: new minimal Hetzner box vs colocate on public-services-host. Lean:
   colocate, it is already the public box; isolate via systemd hardening +
   separate user.
4. CLI-first or dashboard-first for task 7/8 ordering. Lean: CLI first, faster
   verify loop.
5. UDP/QUIC for v2 transport: `webrtc-rs/rtc` (Rust WebRTC, ICE + DTLS data
   channels) or iroh (QUIC, already spec'd) both handle NAT traversal off-Tailscale
   and would eliminate the relay latency for direct paths. Decide before v2 scoping;
   don't design toward it for MVP.
6. mTLS / cert-pinning on the relay TLS connection: adding client cert auth at the
   TLS layer gives a second check before the Noise handshake starts (rejects unknown
   clients at TLS termination). Con: one more cert to manage; pin churn on key
   rotation. Lean: defer to Wave 4 hardening; the relay nonce-auth (§8) already
   rejects unknown BLAKE3 hashes at the application layer, which is sufficient for
   MVP.
7. `op_root` hardware key (Google Cloud KMS / YubiKey): store `op_root` Ed25519
   in an HSM so it never leaves hardware; signing biscuits requires a KMS API call.
   Acceptable latency for 1h TTL refresh. Lean: implement for `op_root` specifically
   in Wave 4; node identity keys stay in sops-nix on the node.

## 13. redshift: biscuit TTL refresh

`wormhole redshift` re-mints the node's biscuit (TTL = now + 1h) and sends it
over the existing Noise session via `Kind::TokenRefresh { token }`. The node
verifies the signature against its cached `op_root_pub` before replacing the
active token and replies with `Kind::TokenAck { expires_at }`.

The operator's refresh loop fires automatically every 50 min (10 min before
expiry) so normal operation never hits the TTL. Manual call is for immediate
revoke-and-reissue. `--all` refreshes every paired node in parallel.

Key invariant: `TokenRefresh` is handled at the auth layer inside wormhole-node
before any `Cmd` dispatch — a node that receives a `Cmd` before accepting a
valid token from a fresh biscuit must reject it.

## 14. lensing: connection diagnostics

`wormhole lensing` is a traceroute for the wormhole session. It sends
`Kind::PathProbe` frames at five standard payload sizes (64 B, 256 B, 1 KB,
4 KB, 16 KB) and collects `Kind::PathProbeReply` from the node.

```
rtt        = reply_received_at_operator - probe.sent_ms
one_way    = rtt / 2  (symmetric path assumption)
```

The 5-size sweep detects relay-side buffering: if `rtt(16 KB) >> rtt(64 B)`
the relay or path is shaping/buffering large frames. On a Tailscale direct
path all sizes should be within 1–2ms of each other.

The operator also displays:
- Active route (from SessionRecord: Relay or Direct)
- biscuit TTL remaining + time until next auto-redshift
- Inbound/outbound seq watermarks and gap status
- Missed Pong count from the heartbeat window (last 5 min)
- Last ntfy notification timestamp

`lensing` is read-only and safe to run at any time during an active session.

## 15. agentic-inbox integration

`agentic-inbox.cabotage.workers.dev` is a deployed Cloudflare Worker behind
**Cloudflare Access Zero Trust**. Its CF Access config already has `mtls_auth`
fields wired — just not yet requiring client certs. Three concrete uses for
wormhole:

### 15a. mTLS via Cloudflare Access (closes §12-6)

Configure the CF Access policy on `agentic-inbox.cabotage.workers.dev` to
require a client certificate from nodes. Each node generates a self-signed
X.509 cert from its `node_ed_pub` key on first start and presents it on every
HTTP request. CF Access validates the cert against a pinned CA (the operator
acts as CA, signs each node cert with `op_root`). No new infra — just CF Access
policy config.

This also applies to the relay host if it's fronted by CF Access: adds a TLS
layer check before the WS handshake even starts, blocking unauthenticated
scanners at the edge.

### 15b. Out-of-band revocation delivery (closes R1-F3)

The security review flagged that revocation lists need a delivery channel that
works even when the wormhole channel is down or compromised.

```
operator  -->  POST /revocation  -->  agentic-inbox (CF Access)
                   body: BLAKE3-signed revocation list

node (on reconnect or hourly poll)  -->  GET /revocation/<node_id_hex>
                   response: signed list (verify against op_root_pub before applying)
```

The inbox stores the latest signed revocation list per node_id. Nodes poll
hourly (not on every reconnect — don't hammer the Worker). The signed list is
the source of truth; the wormhole channel push (§8) is a real-time supplement.

### 15c. Offline command queue

When the operator wants to send a Cmd to a node that is currently offline:

```
operator  -->  POST /queue/<node_id_hex>  -->  agentic-inbox
                   body: Noise-encrypted Envelope (same format as on-wire)
                   header: Authorization: Bearer <cf_service_token>

node (on reconnect)  -->  GET /queue/<node_id_hex>  -->  drain queue
                   node verifies Noise payload, executes in seq order
```

The Worker holds the queue in Durable Objects (ordered, durable). The node
drains on reconnect before processing any live Cmds so ordering is preserved.
Max queue depth: 100 messages; older messages are dropped with a `GapNotice`
equivalent in the API response.

### 15d. Session record backup (closes the "CDN backup" idea)

On every successful handshake the node POSTs the `SessionRecord` to the inbox
as a background fire-and-forget:

```
POST /sessions/<node_id_hex>
body: postcard-encoded SessionRecord
Authorization: Bearer <cf_service_token>
```

Retention: 30 days. The operator can audit `GET /sessions/<node_id_hex>` to
reconstruct fleet topology history. This is metadata (route + timestamps), not
command content — safe to store on a third-party service.

### 15e. Vaultwarden

Use the self-hosted Vaultwarden instance for the **operator side only**:
storing node public keys, the biscuit root key location, and CF service tokens.
Nodes store keys in sops-nix (§3); Vaultwarden is not in the node's trust
boundary. The operator CLI reads node public keys from Vaultwarden on attach
(instead of a local flat file) so key rotation propagates automatically to all
operator clients.

## 16. Supported integration targets (node targets)

wormhole-node is a daemon. It runs on every host that runs a crabcc agent.

| Host class | Arch | Init | Fragment |
|---|---|---|---|
| hetzner / cloud VMs | x86_64 | systemd | `modules/wormhole-node.nix` |
| dev-cx53 | x86_64 | systemd | same |
| mbp / Mac | aarch64 | launchd | `install/integrations/os/launchd-wormhole-node.plist` (Wave 2) |
| pi | aarch64-linux | systemd | cross-compiled via nix; added to `pi.fragment.json` in Wave 2 |
| omp nodes | aarch64/x86_64 | systemd/launchd | added to `omp.fragment.json` in Wave 2 |
| nullclaw | x86_64 | systemd | added to `nullclaw.fragment.json` in Wave 2 |

OpenCode is retired as of v4.5 — not a target.

Cleanup on exit: on SIGTERM the node daemon removes `/tmp/wormhole-*.session`
files it wrote, keeps `~/.crabcc/wormhole-sessions/` records for 72h (pruned on
next start), and flushes any pending log to disk. `crabcc wormhole cleanup` runs
this manually.

## 14. ntfy presence notifications

Side-channel push to `ntfy.crabcc.app` on node connect/disconnect. Three lines
of code in `wormhole-node`; fire-and-forget (failures logged, never propagated).

Topic per node: `wormhole-<node_id_hex8>` (first 8 hex chars of `node_id`).
Topic name is a secret — do not use the default `peter` topic. Rate-limit to
one notification per 60 s per node to degrade timing-correlation attacks (R2-F3).

```http
POST https://ntfy.crabcc.app/wormhole-<node_id_hex8>
Authorization: Bearer <token>       # from sops, NOT node_identity_key
X-Title: <hostname> connected
X-Tags: electric_plug
X-Priority: 3
X-Message: session <session_id_hex8> via relay
```

On disconnect:

```http
X-Title: <hostname> disconnected
X-Tags: no_entry
X-Priority: 2
X-Message: last seq <watermark>
```

The `Authorization` token is a separate ntfy credential — never share the
wormhole identity keys with the ntfy call. Store it in sops as
`secrets/wormhole-ntfy-token.yaml`, distinct from `secrets/wormhole.yaml`.

## 14. Session record persistence

Every successful wormhole handshake (Kind::Hello exchanged) writes a
postcard-serialized `SessionRecord` to disk. This is the canonical session
origin log: it survives relay log truncation, node crashes, and relay compromise.

```rust
struct SessionRecord {
    session: SessionId,
    node_id: [u8; 32],
    op_id:   [u8; 32],
    connected_at: u64,    // Unix seconds, set at Hello
    route: Route,         // Route::Relay{relay_addr} | Route::Direct{peer_addr}
}
```

Write path: `persist_session_record(record, fallback_dir)` in `wormhole-proto`
tries `/tmp/wormhole-<hex8>.session` first, then `fallback_dir/<hex8>.session`.
Default `fallback_dir` for the node daemon: `~/.crabcc/wormhole-sessions/`.

The file is written synchronously (no async) at handshake time so it completes
before the first Cmd is accepted. On reconnect, the existing file is overwritten
(same session ID → same filename → idempotent). Both the node and the operator
side write a record; they will differ in `route` perspective (each sees their own
outbound address) which is intentional.

## 15. Security residuals

Actionable items from two-round adversarial review. Resolve by the wave noted.

| ID | Wave | Severity | Issue | Resolution |
|---|---|---|---|---|
| R1-F1 | W2 | HIGH | SPAKE2 channel binding MAC input unspecified; 2-word code too low entropy | Spec §4 now mandates BLAKE3 full-transcript MAC + 3-word codes |
| R1-F2 | W1 | HIGH | Relay nonce-auth: nonce generation and freshness unspecified | Relay must issue 128-bit random nonces with 30s freshness window; document in relay impl |
| R1-F3 | W4 | HIGH | biscuit TTL 24h; revocation list unsigned; op_root compromise recovery unclear | TTL now 1h; list must be op_root-signed; recovery = re-pair all nodes (§8) |
| R1-F4 | W2 | MEDIUM | SeqState watermark not persisted; crash resets to 0, old frames accepted | wormhole-node must persist watermark to `.crabcc/wormhole/<session>.seq` on every ACK |
| R1-F5 | W3 | MEDIUM | GapNotice from untrusted relay unverifiable; selective drop looks like routine cap | Operator must treat GapNotice as a resume trigger (request from node buffer), not as authoritative |
| R1-F6 | W4 | LOW | Relay key-hash config has no rotation path after op key rotation | Document rotation procedure: relay config update required; add health-check on startup |
| R2-F1 | W1 | MEDIUM | No structural separation between relay-parsed outer header and Noise-protected inner | Resolved: `OuterFrame` type added to proto; relay ONLY decodes OuterFrame |
| R2-F2 | W1 | HIGH | ReplayLog: single oversized frame can evict entire log (gap-forge) | Resolved: `MAX_BODY_BYTES = 64 KiB` constant; relay drops oversized frames; GapNotice includes entry count |
| R2-F3 | W3 | MEDIUM | ntfy real-time presence leaks operator timing via known topic | Resolved in §13: 60s rate-limit; per-node topic names are secrets |
| R2-F4 | W2 | HIGH | sops rotation of node identity key silently breaks channel; no mismatch detection | Resolved in §8: identity key in separate sops entry without restartUnits |
| R2-F5 | W1 | MEDIUM | SessionId entropy source unspecified; weak RNG on embedded hosts | Resolved in code: envelope.rs mandates OsRng; relay rejects duplicate (node_id, session) |
