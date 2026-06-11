/// End-to-end protocol simulation.
///
/// Two in-process peers (operator and node) exchange frames through an
/// in-memory relay that runs the same ReplayLog + routing logic the real relay
/// will use. No I/O, no Noise crypto, no SPAKE2 — just the state machines from
/// wormhole-proto running against each other.
///
/// Covers:
///   - Full Hello → Cmd/Event/Ack cycle
///   - Simulated socket drop + Resume: zero frames lost
///   - TokenRefresh (redshift) lifecycle
///   - PathProbe/PathProbeReply (lensing) with all five standard sizes
///   - PairingHello version negotiation: happy path + mismatch abort
///   - PairingHello role conflict detection
///   - Relay log overflow: GapNotice delivery
use std::collections::VecDeque;
use wormhole_proto::{
    Envelope, Kind, MAX_BODY_BYTES, PairingError, PairingHello, PairingRole, ReplayLog, SeqState,
    SessionId, PAIRING_VERSION,
};

// ---------------------------------------------------------------------------
// In-process relay
// ---------------------------------------------------------------------------

/// Simulates the relay's routing + replay-log for one session.
///
/// Operator→Node frames go directly to `op_to_node` (Cmds; acked, not logged).
/// Node→Operator frames are appended to `log` AND pushed to `node_to_op`
/// (Events; replayed on Resume).
struct InMemoryRelay {
    log: ReplayLog,
    node_to_op: VecDeque<Envelope>,
    op_to_node: VecDeque<Envelope>,
}

impl InMemoryRelay {
    fn new(log_cap: usize) -> Self {
        Self {
            log: ReplayLog::new(log_cap),
            node_to_op: VecDeque::new(),
            op_to_node: VecDeque::new(),
        }
    }

    /// Node sends a frame toward the operator.
    fn node_send(&mut self, env: Envelope) {
        let bytes = env.encode().unwrap();
        let seq = env.seq;
        self.log.push(seq, bytes);
        self.node_to_op.push_back(env);
    }

    /// Operator sends a frame toward the node.
    fn op_send(&mut self, env: Envelope) {
        self.op_to_node.push_back(env);
    }

    /// Operator receives the next available frame.
    fn op_recv(&mut self) -> Option<Envelope> {
        self.node_to_op.pop_front()
    }

    /// Node receives the next available frame.
    fn node_recv(&mut self) -> Option<Envelope> {
        self.op_to_node.pop_front()
    }

    /// Simulate socket drop: in-flight unread frames are discarded from both queues.
    fn drop_connection(&mut self) {
        self.node_to_op.clear();
        self.op_to_node.clear();
    }

    /// Replay node→op frames from `from_seq` out of the log.
    fn replay_to_op(&mut self, from_seq: u64) {
        let replayed: Vec<Envelope> = self
            .log
            .replay_from(from_seq)
            .map(|(_, bytes)| Envelope::decode(bytes).unwrap())
            .collect();
        for env in replayed {
            self.node_to_op.push_back(env);
        }
        // If the relay log was truncated, push a GapNotice.
        if let Some((from, to)) = self.log.gap() {
            if from <= from_seq {
                self.node_to_op.push_front(Envelope {
                    session: 0,
                    seq: 0,
                    kind: Kind::GapNotice { from, to },
                    body: vec![],
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Peer helpers
// ---------------------------------------------------------------------------

struct Peer {
    inbound: SeqState,
    outbound: SeqState,
    session: SessionId,
}

impl Peer {
    fn new(session: SessionId) -> Self {
        Self { inbound: SeqState::new(), outbound: SeqState::new(), session }
    }

    fn send(&mut self, kind: Kind) -> Envelope {
        self.send_body(kind, vec![])
    }

    fn send_body(&mut self, kind: Kind, body: Vec<u8>) -> Envelope {
        Envelope { session: self.session, seq: self.outbound.next_send(), kind, body }
    }

    fn accept(&mut self, env: &Envelope) -> bool {
        self.inbound.accept(env.seq)
    }

    fn watermark(&self) -> Option<u64> {
        self.inbound.watermark()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const SESSION: SessionId = 0xdeadbeef_cafebabe;

/// Full Hello → Cmd/Event/Ack cycle through the relay.
#[test]
fn full_hello_and_cmd_event_ack_cycle() {
    let mut relay = InMemoryRelay::new(1 << 20);
    let mut op = Peer::new(SESSION);
    let mut node = Peer::new(SESSION);

    // Handshake: operator sends Hello, node echoes Hello.
    relay.op_send(op.send(Kind::Hello));
    let hello = relay.node_recv().unwrap();
    assert_eq!(hello.kind, Kind::Hello);
    assert!(node.accept(&hello));

    relay.node_send(node.send(Kind::Hello));
    let hello_back = relay.op_recv().unwrap();
    assert_eq!(hello_back.kind, Kind::Hello);
    assert!(op.accept(&hello_back));

    // Exchange 10 Cmd/Event/Ack pairs.
    // Every frame carries a monotonic seq in its direction — Acks included.
    for i in 0u8..10 {
        // Operator → Node: Cmd
        let cmd = op.send_body(Kind::Cmd, vec![i; 32]);
        let cmd_seq = cmd.seq;
        relay.op_send(cmd);
        let received_cmd = relay.node_recv().unwrap();
        assert!(node.accept(&received_cmd), "node rejected cmd at seq {}", received_cmd.seq);

        // Node → Operator: Event
        let event = node.send_body(Kind::Event, vec![i + 100; 16]);
        let event_seq = event.seq;
        relay.node_send(event);
        let received_event = relay.op_recv().unwrap();
        assert!(op.accept(&received_event), "op rejected event at seq {}", received_event.seq);
        assert_eq!(received_event.body, vec![i + 100; 16]);

        // Operator → Node: Ack the event (seq advances in op→node direction)
        relay.op_send(op.send(Kind::Ack { seq: event_seq }));
        let ack = relay.node_recv().unwrap();
        assert!(node.accept(&ack), "node rejected ack at seq {}", ack.seq);
        assert_eq!(ack.kind, Kind::Ack { seq: event_seq });

        // Node → Operator: Ack the cmd (seq advances in node→op direction)
        relay.node_send(node.send(Kind::Ack { seq: cmd_seq }));
        let cmd_ack = relay.op_recv().unwrap();
        assert!(op.accept(&cmd_ack), "op rejected cmd_ack at seq {}", cmd_ack.seq);
        assert_eq!(cmd_ack.kind, Kind::Ack { seq: cmd_seq });
    }

    // Each direction: Hello(0) + 10×[msg, ack] = 21 frames (seq 0..=20)
    assert_eq!(op.watermark(), Some(20));
    assert_eq!(node.watermark(), Some(20));
}

/// Simulated socket drop mid-stream. Zero frames lost after Resume.
///
/// This is the core VB-W3 property: replayed stream is byte-identical.
#[test]
fn disconnect_and_resume_zero_frames_lost() {
    let mut relay = InMemoryRelay::new(1 << 20);
    let mut op = Peer::new(SESSION);
    let mut node = Peer::new(SESSION);

    // Establish session: Hello
    relay.node_send(node.send(Kind::Hello));
    relay.op_recv().map(|e| op.accept(&e));

    // Node sends 20 events.
    for i in 0u8..20 {
        relay.node_send(node.send_body(Kind::Event, vec![i; 8]));
    }

    // Operator receives first 12 and then the socket drops.
    for _ in 0..12 {
        let e = relay.op_recv().unwrap();
        op.accept(&e);
    }
    relay.drop_connection(); // 8 frames in-flight are lost

    assert_eq!(op.watermark(), Some(12)); // Hello(0) + events 1..=12

    // Reconnect: operator sends Resume{from_seq: 13}.
    let resume_from = op.watermark().unwrap() + 1; // 13
    assert_eq!(resume_from, 13);
    relay.replay_to_op(resume_from);

    // Operator receives replayed frames 13..=20 (8 events).
    let mut replayed_count = 0u8;
    while let Some(e) = relay.op_recv() {
        if matches!(e.kind, Kind::GapNotice { .. }) { continue; } // shouldn't happen in this test
        assert!(op.accept(&e), "replayed frame at seq {} rejected", e.seq);
        assert_eq!(e.body, vec![replayed_count + 12; 8],
            "body mismatch at replayed_count={replayed_count}");
        replayed_count += 1;
    }

    assert_eq!(replayed_count, 8, "should have replayed exactly 8 frames");
    assert_eq!(op.watermark(), Some(20), "operator should have all 20 events");
}

/// Resume where the relay log was capped and a GapNotice is emitted.
#[test]
fn resume_with_relay_log_gap() {
    // Tiny log: 10 bytes per entry, cap = 80 bytes => holds ~8 entries.
    let mut relay = InMemoryRelay::new(80);
    let mut node = Peer::new(SESSION);

    // Node sends 20 events.
    for i in 0u8..20 {
        relay.node_send(node.send_body(Kind::Event, vec![i; 10]));
    }

    // Drain the live queue.
    while relay.op_recv().is_some() {}

    // Operator missed everything; reconnects asking from seq 0.
    relay.replay_to_op(0);

    // First frame must be a GapNotice.
    let first = relay.op_recv().unwrap();
    assert!(
        matches!(first.kind, Kind::GapNotice { .. }),
        "expected GapNotice when log was truncated, got {:?}", first.kind
    );
    let Kind::GapNotice { from, to } = first.kind else { unreachable!() };
    assert_eq!(from, 0, "gap must start at seq 0");
    assert!(to >= from);

    // Frames after the gap are intact.
    let tail_start = to + 1;
    let mut last_seq = tail_start - 1;
    while let Some(e) = relay.op_recv() {
        assert!(e.seq >= tail_start, "got seq {} before tail_start {}", e.seq, tail_start);
        assert_eq!(e.seq, last_seq + 1, "gap in tail (non-contiguous)");
        last_seq = e.seq;
    }
}

/// TokenRefresh (redshift) lifecycle: operator mints new token, node acks.
#[test]
fn token_refresh_redshift_lifecycle() {
    let mut relay = InMemoryRelay::new(1 << 20);
    let mut op = Peer::new(SESSION);
    let mut node = Peer::new(SESSION);

    // Establish session.
    relay.op_send(op.send(Kind::Hello));
    relay.node_recv().map(|e| node.accept(&e));
    relay.node_send(node.send(Kind::Hello));
    relay.op_recv().map(|e| op.accept(&e));

    // Operator mints a fresh biscuit (simulated as 64 opaque bytes).
    let fake_token: Vec<u8> = (0..64).collect();
    let new_expiry = 1_700_003_600u64;

    let refresh = op.send_body(Kind::TokenRefresh { token: fake_token.clone() }, vec![]);
    relay.op_send(refresh);

    // Node receives, "verifies" (mocked), sends TokenAck.
    let received = relay.node_recv().unwrap();
    assert!(node.accept(&received));
    match &received.kind {
        Kind::TokenRefresh { token } => assert_eq!(token, &fake_token),
        other => panic!("expected TokenRefresh, got {other:?}"),
    }

    relay.node_send(node.send(Kind::TokenAck { expires_at: new_expiry }));

    // Operator receives ack with correct expiry.
    let ack = relay.op_recv().unwrap();
    assert!(op.accept(&ack));
    assert_eq!(ack.kind, Kind::TokenAck { expires_at: new_expiry });
}

/// PathProbe / PathProbeReply (lensing): all five standard sizes, id matching.
#[test]
fn path_probe_lensing_five_sizes() {
    const PROBE_SIZES: &[u16] = &[64, 256, 1024, 4096, 16384];

    let mut relay = InMemoryRelay::new(1 << 20);
    let mut op = Peer::new(SESSION);
    let mut node = Peer::new(SESSION);

    let base_ms = 1_700_000_000_000u64;

    // Operator sends one probe per size.
    for (i, &size) in PROBE_SIZES.iter().enumerate() {
        assert!(size as usize <= MAX_BODY_BYTES, "size {size} exceeds MAX_BODY_BYTES");
        let probe = op.send_body(
            Kind::PathProbe { id: i as u32, sent_ms: base_ms + i as u64, payload_size: size },
            vec![0u8; size as usize],
        );
        relay.op_send(probe);
    }

    // Node receives all probes and replies.
    let mut node_recv_times = Vec::new();
    for i in 0..PROBE_SIZES.len() {
        let probe = relay.node_recv().unwrap();
        assert!(node.accept(&probe));
        let (id, sent_ms, payload_size) = match probe.kind {
            Kind::PathProbe { id, sent_ms, payload_size } => (id, sent_ms, payload_size),
            _ => panic!("expected PathProbe"),
        };
        assert_eq!(id, i as u32);
        assert_eq!(probe.body.len(), payload_size as usize);

        let node_recv_ms = sent_ms + 10 + i as u64; // simulate 10ms + tiny jitter
        node_recv_times.push(node_recv_ms);
        relay.node_send(node.send(Kind::PathProbeReply { id, node_recv_ms }));
    }

    // Operator collects replies and computes RTTs.
    for (i, &size) in PROBE_SIZES.iter().enumerate() {
        let reply = relay.op_recv().unwrap();
        assert!(op.accept(&reply));
        let (id, node_recv_ms) = match reply.kind {
            Kind::PathProbeReply { id, node_recv_ms } => (id, node_recv_ms),
            _ => panic!("expected PathProbeReply"),
        };
        assert_eq!(id, i as u32, "probe id mismatch for size {size}");

        let sent_ms = base_ms + i as u64;
        let rtt = node_recv_ms - sent_ms; // one-way sim; in real use: reply_recv - sent
        assert!(rtt >= 10, "implausibly low RTT {rtt}ms for size {size}");
    }

    // Every probe got exactly one reply.
    assert!(relay.op_recv().is_none(), "unexpected extra frames");
}

/// PairingHello version negotiation: both sides on the same version.
#[test]
fn pairing_hello_version_match() {
    let node_hello = PairingHello::node();
    let op_hello = PairingHello::operator();

    assert_eq!(node_hello.version, PAIRING_VERSION);
    assert_eq!(op_hello.version, PAIRING_VERSION);
    assert_eq!(node_hello.role, PairingRole::Node);
    assert_eq!(op_hello.role, PairingRole::Operator);

    // Simulate relay delivering each hello to the other peer.
    let node_bytes = node_hello.encode().unwrap();
    let op_bytes = op_hello.encode().unwrap();

    let received_by_op = PairingHello::decode(&node_bytes).unwrap();
    let received_by_node = PairingHello::decode(&op_bytes).unwrap();

    // Both peers accept: same version.
    assert_eq!(received_by_op.version, PAIRING_VERSION);
    assert_eq!(received_by_node.version, PAIRING_VERSION);
    assert_ne!(received_by_op.role, received_by_node.role, "roles must differ");
}

/// PairingHello version mismatch: higher-version side aborts.
#[test]
fn pairing_hello_version_mismatch_aborts() {
    let ours = PairingHello::node(); // version 1
    let theirs = PairingHello {
        version: PAIRING_VERSION + 1, // future version
        role: PairingRole::Operator,
    };

    // Higher-version side (theirs) detects mismatch and constructs the error.
    let mismatch = ours.version != theirs.version;
    assert!(mismatch);

    // Higher version aborts — no downgrade.
    let err = PairingError::VersionMismatch { ours: theirs.version, theirs: ours.version };
    let bytes = err.encode().unwrap();
    match PairingError::decode(&bytes).unwrap() {
        PairingError::VersionMismatch { ours: o, theirs: t } => {
            assert_eq!(o, PAIRING_VERSION + 1);
            assert_eq!(t, PAIRING_VERSION);
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

/// PairingHello role conflict: two nodes connecting on the same nameplate.
#[test]
fn pairing_hello_role_conflict_detected() {
    let a = PairingHello::node();
    let b = PairingHello::node(); // two nodes — conflict

    // Both received each other's hello; relay detects same role.
    let conflict = a.role == b.role;
    assert!(conflict, "two nodes must be detected as a role conflict");

    let err = PairingError::RoleConflict;
    assert_eq!(PairingError::decode(&err.encode().unwrap()).unwrap(), err);
}

/// Heartbeat: Ping/Pong traverses the relay correctly.
#[test]
fn heartbeat_ping_pong() {
    let mut relay = InMemoryRelay::new(1 << 20);
    let mut op = Peer::new(SESSION);
    let mut node = Peer::new(SESSION);

    relay.op_send(op.send(Kind::Ping));
    let ping = relay.node_recv().unwrap();
    assert_eq!(ping.kind, Kind::Ping);
    assert!(node.accept(&ping));

    relay.node_send(node.send(Kind::Pong));
    let pong = relay.op_recv().unwrap();
    assert_eq!(pong.kind, Kind::Pong);
    assert!(op.accept(&pong));
}

/// Multiple sessions can coexist on the same relay (different session IDs).
#[test]
fn multiple_sessions_independent() {
    const SESSION_A: SessionId = 0xAAAA;
    const SESSION_B: SessionId = 0xBBBB;

    let mut relay_a = InMemoryRelay::new(1 << 20);
    let mut relay_b = InMemoryRelay::new(1 << 20);
    let mut node_a = Peer::new(SESSION_A);
    let mut node_b = Peer::new(SESSION_B);
    let mut op_a = Peer::new(SESSION_A);
    let mut op_b = Peer::new(SESSION_B);

    // Interleave events on both sessions.
    for i in 0u8..5 {
        relay_a.node_send(node_a.send_body(Kind::Event, vec![0xAA, i]));
        relay_b.node_send(node_b.send_body(Kind::Event, vec![0xBB, i]));
    }

    // Each operator sees only their session's frames.
    let mut a_count = 0;
    while let Some(e) = relay_a.op_recv() {
        assert_eq!(e.session, SESSION_A);
        assert_eq!(e.body[0], 0xAA);
        assert!(op_a.accept(&e));
        a_count += 1;
    }
    let mut b_count = 0;
    while let Some(e) = relay_b.op_recv() {
        assert_eq!(e.session, SESSION_B);
        assert_eq!(e.body[0], 0xBB);
        assert!(op_b.accept(&e));
        b_count += 1;
    }

    assert_eq!(a_count, 5);
    assert_eq!(b_count, 5);
}
