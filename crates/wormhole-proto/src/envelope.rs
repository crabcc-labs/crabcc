use serde::{Deserialize, Serialize};

/// Logical session identifier, minted by the node on session create.
///
/// MUST be generated from a cryptographically secure RNG (e.g. `OsRng` from
/// the `rand` crate or `getrandom`). Never use timestamps, PIDs, or counters.
/// Weak entropy on first-boot embedded hosts (RPi, cloud VMs) can make this
/// guessable; see security residuals §15.
pub type SessionId = u128;

/// Hard cap on `Envelope::body` length enforced before relay log append.
/// Prevents a single oversized frame from evicting the entire replay log
/// (ReplayLog gap-forging, security residual R2-F2).
pub const MAX_BODY_BYTES: usize = 65_536;

/// Frame kind. Variants use named fields so postcard forward-compatibility
/// holds: adding a field with a default won't break existing decoders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Kind {
    /// First frame from the initiator after the Noise handshake completes.
    Hello,
    /// Operator requests replay of events starting at `from_seq` (inclusive).
    Resume {
        from_seq: u64,
    },
    /// Operator-to-node command. Body is opaque to the relay.
    Cmd,
    /// Node-to-operator event. Body is opaque to the relay.
    Event,
    /// Positive acknowledgment of the frame with `seq`.
    Ack {
        seq: u64,
    },
    Ping,
    Pong,
    /// Relay log was truncated; frames `from..=to` are unavailable.
    /// Operator must handle this as a gap in the event stream.
    GapNotice {
        from: u64,
        to: u64,
    },
    /// Relay-pushed node presence update. `node_id` is BLAKE3(node_static_pub).
    Presence {
        node_id: [u8; 32],
        connected: bool,
        last_seen: u64,
    },

    // ---- redshift: biscuit TTL refresh without re-pairing ----
    /// Operator re-mints the node's biscuit (new TTL = now + 1h) and sends it
    /// over the existing Noise session. The node MUST verify the signature
    /// against its cached `op_root_pub` before replacing the active token.
    /// Fired automatically every 50 min by the operator's refresh loop.
    TokenRefresh {
        /// Serialized biscuit token bytes (opaque to the relay).
        token: Vec<u8>,
    },
    /// Node confirms the new token is installed and reports its expiry.
    TokenAck {
        /// Expiry of the newly active token, Unix seconds.
        expires_at: u64,
    },

    // ---- lensing: connection diagnostics / "traceroute" ----
    /// Operator-initiated path probe. Body is padded to `payload_size` zero
    /// bytes so RTT can be measured at different effective payload sizes
    /// (detects queuing/shaping at the relay or on-path buffers).
    /// Five standard sizes: 64 B, 256 B, 1 KB, 4 KB, 16 KB.
    PathProbe {
        /// Discriminator matching reply to request.
        id: u32,
        /// Operator-side send timestamp, Unix milliseconds.
        sent_ms: u64,
        /// Requested body padding (0..=MAX_BODY_BYTES). Operator sets body len
        /// to this value; node ignores body content on reply.
        payload_size: u16,
    },
    /// Node's reply to a PathProbe. Operator computes:
    ///   rtt = (reply_received_ms - sent_ms)
    ///   one_way_estimate = rtt / 2  (symmetric path assumption)
    PathProbeReply {
        /// Echo of the probe's `id`.
        id: u32,
        /// Node-side receive timestamp, Unix milliseconds.
        node_recv_ms: u64,
    },
}

/// Inner envelope, postcard-serialized inside a Noise transport message.
///
/// The relay never decrypts this; it only reads the outer routing header
/// (node_id, channel) which lives outside the Noise payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Session this frame belongs to.
    pub session: SessionId,
    /// Per-session monotonic counter, sender-assigned. Checked monotonic
    /// across reconnects for replay protection, independent of Noise nonces.
    pub seq: u64,
    pub kind: Kind,
    /// Opaque payload. Non-empty only for Cmd and Event.
    pub body: Vec<u8>,
}

impl Envelope {
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

/// Outer WebSocket frame parsed by the relay.
///
/// The relay deserialises only this type — it never touches `Envelope`. This
/// structural separation prevents type-confusion between relay-parsed metadata
/// and Noise-protected inner content (security residual R2-F1). `noise_payload`
/// is forwarded verbatim without inspection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OuterFrame {
    /// BLAKE3(node_static_pub) — routing key, only identifier the relay sees.
    pub node_id: [u8; 32],
    /// Logical sub-channel within a node connection (0 = control, 1+ = sessions).
    pub channel: u16,
    /// Noise transport message bytes. Opaque to the relay.
    pub noise_payload: Vec<u8>,
}

impl OuterFrame {
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

/// How a session was routed on connect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Route {
    /// Session reached the node through the public relay.
    Relay { relay_addr: String },
    /// Direct connection via Tailscale (fast path).
    Direct { peer_addr: String },
}

/// Session-origin record written to disk on every successful wormhole
/// handshake (Kind::Hello exchanged). Postcard-serialized.
///
/// The node daemon writes this to `/tmp/wormhole-<hex8>.session` and falls
/// back to `~/.crabcc/wormhole-sessions/<hex8>.session` if /tmp is not
/// writable. The I/O lives in `wormhole-node`; this crate only defines the
/// type and its serialization.
///
/// Rationale: even if the node crashes or the relay loses its log, this record
/// preserves the session origin (route, identities, timestamp) so the operator
/// can reconstruct what was reachable and when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session: SessionId,
    /// BLAKE3(node_static_pub) — the node's routing identity.
    pub node_id: [u8; 32],
    /// BLAKE3(op_static_pub) — operator identity at connect time.
    pub op_id: [u8; 32],
    /// Unix timestamp (seconds) when Kind::Hello was successfully exchanged.
    pub connected_at: u64,
    /// Route taken to establish this session.
    pub route: Route,
}

impl SessionRecord {
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }

    /// Suggested filename for the on-disk record.
    /// Uses the low 32 bits of the session ID as a short discriminator.
    pub fn filename(&self) -> String {
        format!(
            "wormhole-{:08x}.session",
            (self.session & 0xffff_ffff) as u32
        )
    }
}

/// Write `record` to disk: try `/tmp/<filename>` first, fall back to
/// `fallback_dir/<filename>`. Returns the path actually written.
///
/// This function is intentionally pure-std (no tokio) so it can be called
/// from a sync context at handshake time without spawning a task.
pub fn persist_session_record(
    record: &SessionRecord,
    fallback_dir: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    let filename = record.filename();
    let bytes = record
        .encode()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let tmp_path = std::path::Path::new("/tmp").join(&filename);
    if std::fs::write(&tmp_path, &bytes).is_ok() {
        return Ok(tmp_path);
    }

    std::fs::create_dir_all(fallback_dir)?;
    let fallback_path = fallback_dir.join(&filename);
    std::fs::write(&fallback_path, &bytes)?;
    Ok(fallback_path)
}
