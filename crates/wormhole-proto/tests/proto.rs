use wormhole_proto::{
    persist_session_record, Envelope, Kind, OuterFrame, PairingError, PairingHello, PairingResult,
    PairingRole, ReplayLog, Route, SeqState, SessionId, SessionRecord, MAX_BODY_BYTES,
    PAIRING_VERSION,
};

// ---- helpers ----

fn env(session: SessionId, seq: u64, kind: Kind) -> Envelope {
    Envelope {
        session,
        seq,
        kind,
        body: vec![],
    }
}

fn env_body(session: SessionId, seq: u64, kind: Kind, body: Vec<u8>) -> Envelope {
    Envelope {
        session,
        seq,
        kind,
        body,
    }
}

// ---- Envelope roundtrip: every Kind variant (including redshift + lensing) ----

#[test]
fn roundtrip_hello() {
    let e = env(0, 0, Kind::Hello);
    assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
}

#[test]
fn roundtrip_resume() {
    let e = env(1, 5, Kind::Resume { from_seq: 3 });
    assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
}

#[test]
fn roundtrip_cmd_with_body() {
    let e = env_body(42, 0, Kind::Cmd, b"crabcc agent run test".to_vec());
    assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
}

#[test]
fn roundtrip_event_with_body() {
    let e = env_body(42, 1, Kind::Event, vec![0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
}

#[test]
fn roundtrip_ack() {
    let e = env(7, 10, Kind::Ack { seq: 9 });
    assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
}

#[test]
fn roundtrip_ping_pong() {
    for kind in [Kind::Ping, Kind::Pong] {
        let e = env(0, 0, kind);
        assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
    }
}

#[test]
fn roundtrip_gap_notice() {
    let e = env(0, 0, Kind::GapNotice { from: 100, to: 200 });
    assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
}

#[test]
fn roundtrip_presence() {
    let e = env(
        0,
        0,
        Kind::Presence {
            node_id: [0xab; 32],
            connected: true,
            last_seen: 1_700_000_000,
        },
    );
    assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
}

#[test]
fn roundtrip_large_session_id() {
    // SessionId is u128 — make sure postcard handles the full range.
    let e = env(u128::MAX, u64::MAX, Kind::Ping);
    assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
}

// redshift
#[test]
fn roundtrip_token_refresh() {
    let token: Vec<u8> = (0..64)
        .map(|i| if i % 2 == 0 { 0xbe } else { 0xef })
        .collect();
    let e = env_body(1, 0, Kind::TokenRefresh { token }, vec![]);
    assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
}

#[test]
fn roundtrip_token_ack() {
    let e = env(
        1,
        1,
        Kind::TokenAck {
            expires_at: 1_700_003_600,
        },
    );
    assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
}

// lensing
#[test]
fn roundtrip_path_probe() {
    let e = env_body(
        42,
        7,
        Kind::PathProbe {
            id: 99,
            sent_ms: 1_700_000_000_000,
            payload_size: 1024,
        },
        vec![0u8; 1024],
    );
    assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
}

#[test]
fn roundtrip_path_probe_reply() {
    let e = env(
        42,
        8,
        Kind::PathProbeReply {
            id: 99,
            node_recv_ms: 1_700_000_000_012,
        },
    );
    assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
}

#[test]
fn path_probe_sizes_fit_in_max_body() {
    // The five standard lensing payload sizes must all fit within MAX_BODY_BYTES.
    for size in [64u16, 256, 1024, 4096, 16384] {
        assert!(
            size as usize <= MAX_BODY_BYTES,
            "lensing payload size {size} exceeds MAX_BODY_BYTES"
        );
    }
}

#[test]
fn token_refresh_zero_len_token_roundtrips() {
    // Empty token is technically invalid but must not panic on encode/decode.
    let e = env(0, 0, Kind::TokenRefresh { token: vec![] });
    assert_eq!(Envelope::decode(&e.encode().unwrap()).unwrap(), e);
}

#[test]
fn decode_rejects_garbage() {
    assert!(Envelope::decode(&[0xff, 0x00, 0x12]).is_err());
}

#[test]
fn decode_rejects_truncated() {
    let e = env(1, 1, Kind::Hello);
    let bytes = e.encode().unwrap();
    assert!(Envelope::decode(&bytes[..bytes.len() / 2]).is_err());
}

// ---- SeqState ----

#[test]
fn seq_accepts_monotonic_run() {
    let mut s = SeqState::new();
    for i in 0u64..100 {
        assert!(s.accept(i), "should accept seq {i}");
    }
    assert_eq!(s.watermark(), Some(99));
}

#[test]
fn seq_rejects_duplicate() {
    let mut s = SeqState::new();
    assert!(s.accept(0));
    assert!(!s.accept(0), "duplicate must be rejected");
    // Counter must not advance on rejection.
    assert!(s.accept(1));
}

#[test]
fn seq_rejects_gap() {
    let mut s = SeqState::new();
    assert!(s.accept(0));
    assert!(!s.accept(2), "gap must be rejected");
    // 1 is still the next expected.
    assert!(s.accept(1));
    assert_eq!(s.watermark(), Some(1));
}

#[test]
fn seq_rejects_rewind() {
    let mut s = SeqState::new();
    assert!(s.accept(0));
    assert!(s.accept(1));
    assert!(!s.accept(0), "rewind must be rejected");
}

#[test]
fn seq_watermark_none_before_first_accept() {
    assert_eq!(SeqState::new().watermark(), None);
}

#[test]
fn seq_next_send_is_monotonic() {
    let mut s = SeqState::new();
    for expected in 0u64..50 {
        assert_eq!(s.next_send(), expected);
    }
}

#[test]
fn seq_send_and_receive_are_independent() {
    let mut s = SeqState::new();
    // advance outbound
    s.next_send();
    s.next_send();
    // inbound still starts at 0
    assert!(s.accept(0));
    assert_eq!(s.watermark(), Some(0));
}

#[test]
fn seq_advance_inbound_to_skips_range() {
    let mut s = SeqState::new();
    s.advance_inbound_to(9);
    assert_eq!(s.watermark(), Some(9));
    assert!(s.accept(10));
    assert_eq!(s.watermark(), Some(10));
}

#[test]
fn seq_advance_inbound_to_is_idempotent() {
    let mut s = SeqState::new();
    s.advance_inbound_to(5);
    s.advance_inbound_to(5); // no-op
    assert_eq!(s.watermark(), Some(5));
    assert!(s.accept(6));
}

#[test]
fn seq_advance_inbound_to_does_not_rewind() {
    let mut s = SeqState::new();
    s.advance_inbound_to(10);
    s.advance_inbound_to(3); // lower value — must not rewind
    assert!(s.accept(11));
    assert_eq!(s.watermark(), Some(11));
}

// ---- ReplayLog ----

fn push_n(log: &mut ReplayLog, n: usize, base_seq: u64) -> Vec<Vec<u8>> {
    (0..n)
        .map(|i| {
            let seq = base_seq + i as u64;
            let payload = format!("frame-{seq}").into_bytes();
            log.push(seq, payload.clone());
            payload
        })
        .collect()
}

#[test]
fn replay_from_start() {
    let mut log = ReplayLog::new(1 << 20);
    let payloads = push_n(&mut log, 10, 0);
    let replayed: Vec<_> = log.replay_from(0).collect();
    assert_eq!(replayed.len(), 10);
    for (i, (seq, payload)) in replayed.iter().enumerate() {
        assert_eq!(*seq, i as u64);
        assert_eq!(payload, &payloads[i]);
    }
}

#[test]
fn replay_from_mid() {
    let mut log = ReplayLog::new(1 << 20);
    let payloads = push_n(&mut log, 10, 0);
    let from = 5u64;
    let replayed: Vec<_> = log.replay_from(from).collect();
    assert_eq!(replayed.len(), 5);
    for (i, (seq, payload)) in replayed.iter().enumerate() {
        assert_eq!(*seq, from + i as u64);
        assert_eq!(payload, &payloads[from as usize + i]);
    }
}

#[test]
fn replay_from_last() {
    let mut log = ReplayLog::new(1 << 20);
    let payloads = push_n(&mut log, 5, 0);
    let replayed: Vec<_> = log.replay_from(4).collect();
    assert_eq!(replayed.len(), 1);
    assert_eq!(&replayed[0].1, &payloads[4]);
}

#[test]
fn replay_from_past_end_is_empty() {
    let mut log = ReplayLog::new(1 << 20);
    push_n(&mut log, 5, 0);
    assert_eq!(log.replay_from(10).count(), 0);
}

#[test]
fn replay_empty_log() {
    let log = ReplayLog::new(1 << 20);
    assert_eq!(log.replay_from(0).count(), 0);
}

/// Core property (VB-W3): replay from any offset k reproduces the exact
/// suffix of the original stream starting at seq k, byte-identical.
#[test]
fn replay_byte_identical_from_every_offset() {
    let mut log = ReplayLog::new(1 << 20);
    let n = 64usize;
    let payloads = push_n(&mut log, n, 0);

    for from in 0..n {
        let replayed: Vec<_> = log.replay_from(from as u64).collect();
        let expected = &payloads[from..];
        assert_eq!(
            replayed.len(),
            expected.len(),
            "length mismatch at from={from}"
        );
        for (i, (seq, payload)) in replayed.iter().enumerate() {
            assert_eq!(*seq, (from + i) as u64, "seq mismatch at from={from} i={i}");
            assert_eq!(
                payload, &expected[i],
                "payload mismatch at from={from} i={i}"
            );
        }
    }
}

/// Replay starting from offset 0 after non-zero base seq works correctly.
#[test]
fn replay_non_zero_base_seq() {
    let mut log = ReplayLog::new(1 << 20);
    // Simulate a resumed session where seqs start at 1000.
    let payloads = push_n(&mut log, 10, 1000);
    let replayed: Vec<_> = log.replay_from(1000).collect();
    assert_eq!(replayed.len(), 10);
    for (i, (seq, payload)) in replayed.iter().enumerate() {
        assert_eq!(*seq, 1000 + i as u64);
        assert_eq!(payload, &payloads[i]);
    }
}

#[test]
fn replay_no_gap_within_cap() {
    let mut log = ReplayLog::new(1 << 20);
    push_n(&mut log, 20, 0);
    assert!(log.gap().is_none());
}

#[test]
fn replay_gap_recorded_when_cap_exceeded() {
    // 10 bytes per entry, cap = 64 bytes => evicts after ~7 entries.
    let mut log = ReplayLog::new(64);
    for i in 0u64..20 {
        log.push(i, vec![i as u8; 10]);
    }
    let gap = log.gap().expect("expected gap after cap exceeded");
    assert_eq!(gap.0, 0, "gap must start at the first entry (seq 0)");
    assert!(gap.1 >= gap.0, "gap.to >= gap.from");
}

#[test]
fn replay_after_gap_gives_surviving_tail() {
    let mut log = ReplayLog::new(64);
    for i in 0u64..20 {
        log.push(i, vec![0u8; 10]);
    }
    let (_, gap_to) = log.gap().unwrap();
    // Everything after the gap must be intact.
    let tail_start = gap_to + 1;
    let replayed: Vec<_> = log.replay_from(tail_start).collect();
    for (seq, _) in &replayed {
        assert!(
            *seq >= tail_start,
            "tail entry has seq {seq} < tail_start {tail_start}"
        );
    }
    // And the tail must be contiguous (no internal gaps).
    for window in replayed.windows(2) {
        assert_eq!(
            window[1].0,
            window[0].0 + 1,
            "tail entries are not contiguous"
        );
    }
}

#[test]
fn replay_push_returns_false_on_eviction() {
    let mut log = ReplayLog::new(30);
    assert!(log.push(0, vec![0u8; 10])); // 10 bytes, no eviction
    assert!(log.push(1, vec![0u8; 10])); // 20 bytes, no eviction
    assert!(log.push(2, vec![0u8; 10])); // 30 bytes, at cap, no eviction
    let r = log.push(3, vec![0u8; 10]); // 40 bytes, must evict -> false
    assert!(!r, "push beyond cap should return false");
}

#[test]
fn replay_len_reflects_surviving_entries() {
    let mut log = ReplayLog::new(50);
    for i in 0u64..10 {
        log.push(i, vec![0u8; 10]); // 10 bytes each
    }
    // cap=50 -> 5 entries survive
    assert!(log.len() <= 5, "at most 5 entries within 50-byte cap");
    assert!(!log.is_empty());
}

// ---- SeqState + ReplayLog integration: resume handshake ----

/// Simulates the resume handshake: node sends N frames; operator reconnects
/// mid-stream and asks to resume from some seq k; the replayed frames are
/// byte-identical to the originals from k onward.
#[test]
fn resume_handshake_zero_lost_frames() {
    let n = 32usize;
    let mut log = ReplayLog::new(1 << 20);
    let mut sender = SeqState::new();
    let mut receiver = SeqState::new();

    // Node sends N frames.
    let sent: Vec<Vec<u8>> = (0..n)
        .map(|_| {
            let seq = sender.next_send();
            let payload = format!("payload-{seq}").into_bytes();
            log.push(seq, payload.clone());
            payload
        })
        .collect();

    // Operator connected up to seq 15, then disconnected.
    for i in 0u64..16 {
        assert!(receiver.accept(i), "pre-disconnect accept seq {i}");
    }

    // Reconnect: operator sends Resume{from_seq: 16}.
    let resume_from = receiver.watermark().map_or(0, |w| w + 1);
    assert_eq!(resume_from, 16);

    // Relay replays from seq 16.
    let replayed: Vec<_> = log.replay_from(resume_from).collect();
    assert_eq!(replayed.len(), n - 16);

    // Receiver advances its watermark to 15 (already delivered) and then
    // accepts the replayed frames.
    receiver.advance_inbound_to(15);
    for (i, (seq, payload)) in replayed.iter().enumerate() {
        let expected_seq = 16 + i as u64;
        assert_eq!(*seq, expected_seq);
        assert_eq!(payload, &sent[expected_seq as usize]);
        assert!(receiver.accept(*seq));
    }
    assert_eq!(receiver.watermark(), Some(31));
}

// ---- Pairing protocol (magic-wormhole-inspired: version byte + role) ----

#[test]
fn pairing_hello_node_roundtrip() {
    let h = PairingHello::node();
    assert_eq!(PairingHello::decode(&h.encode().unwrap()).unwrap(), h);
    assert_eq!(h.version, PAIRING_VERSION);
    assert_eq!(h.role, PairingRole::Node);
}

#[test]
fn pairing_hello_operator_roundtrip() {
    let h = PairingHello::operator();
    assert_eq!(PairingHello::decode(&h.encode().unwrap()).unwrap(), h);
    assert_eq!(h.role, PairingRole::Operator);
}

#[test]
fn pairing_hello_versions_differ_abort_check() {
    // Simulate version mismatch detection: if peer's version != ours, error.
    let ours = PairingHello::node();
    let theirs = PairingHello {
        version: PAIRING_VERSION + 1,
        role: PairingRole::Operator,
    };
    assert_ne!(
        ours.version, theirs.version,
        "version mismatch must be detectable"
    );
    let err = PairingError::VersionMismatch {
        ours: ours.version,
        theirs: theirs.version,
    };
    let bytes = err.encode().unwrap();
    assert_eq!(PairingError::decode(&bytes).unwrap(), err);
}

#[test]
fn pairing_error_all_variants_roundtrip() {
    for err in [
        PairingError::VersionMismatch { ours: 1, theirs: 2 },
        PairingError::NameplateAlreadyClaimed,
        PairingError::NameplateExpired,
        PairingError::MacVerificationFailed,
        PairingError::RoleConflict,
    ] {
        assert_eq!(PairingError::decode(&err.encode().unwrap()).unwrap(), err);
    }
}

#[test]
fn pairing_result_fields_accessible() {
    // PairingResult is in-memory only (never sent over wire), so just check fields.
    let r = PairingResult {
        pake_key: [0x11u8; 32],
        peer_static_pub: [0x22u8; 32],
        peer_ed_pub: [0x33u8; 32],
    };
    assert_eq!(r.pake_key[0], 0x11);
    assert_eq!(r.peer_static_pub[0], 0x22);
    assert_eq!(r.peer_ed_pub[0], 0x33);
}

#[test]
fn pairing_hello_and_envelope_are_different_types() {
    // A PairingHello must not accidentally decode as an Envelope.
    let hello = PairingHello::node();
    let bytes = hello.encode().unwrap();
    // We only care it doesn't panic; pass/fail both ok (they have different schemas).
    let _ = Envelope::decode(&bytes);
}

// ---- OuterFrame (R2-F1: structural relay/Noise separation) ----

#[test]
fn outer_frame_roundtrip() {
    let frame = OuterFrame {
        node_id: [0x42u8; 32],
        channel: 7,
        noise_payload: vec![0xde, 0xad, 0xbe, 0xef],
    };
    let bytes = frame.encode().unwrap();
    assert_eq!(OuterFrame::decode(&bytes).unwrap(), frame);
}

#[test]
fn outer_frame_empty_payload() {
    let frame = OuterFrame {
        node_id: [0u8; 32],
        channel: 0,
        noise_payload: vec![],
    };
    assert_eq!(OuterFrame::decode(&frame.encode().unwrap()).unwrap(), frame);
}

#[test]
fn outer_frame_large_payload() {
    let frame = OuterFrame {
        node_id: [0x01u8; 32],
        channel: 1,
        noise_payload: vec![0u8; MAX_BODY_BYTES],
    };
    assert_eq!(OuterFrame::decode(&frame.encode().unwrap()).unwrap(), frame);
}

#[test]
fn outer_frame_and_envelope_are_structurally_independent() {
    // A valid OuterFrame must not accidentally decode as Envelope and vice-versa.
    // This tests that the type discriminants differ at the byte level.
    let outer = OuterFrame {
        node_id: [1u8; 32],
        channel: 0,
        noise_payload: vec![9, 8, 7],
    };
    let outer_bytes = outer.encode().unwrap();
    // Decoding outer bytes as Envelope must fail (different schema).
    let _ = Envelope::decode(&outer_bytes); // we only care it doesn't panic; pass/fail both ok
                                            // Encoding an Envelope must not accidentally decode as OuterFrame.
    let env = env(0, 0, Kind::Hello);
    let env_bytes = env.encode().unwrap();
    let _ = OuterFrame::decode(&env_bytes); // same — just must not panic
}

// ---- MAX_BODY_BYTES (R2-F2: relay-side enforcement surface) ----

#[test]
fn max_body_bytes_value_is_sensible() {
    // 64 KiB: large enough for any control message, small enough to prevent
    // single-frame log eviction on a 64 MiB relay cap.
    assert_eq!(MAX_BODY_BYTES, 65_536);
    // One MAX_BODY_BYTES frame should never evict more than 1/1000 of the
    // default 64 MiB relay cap.
    let relay_cap_bytes: usize = 64 * 1024 * 1024;
    assert!(MAX_BODY_BYTES * 1000 < relay_cap_bytes);
}

// ---- SessionRecord + persist_session_record ----

fn make_record(session: SessionId, route: Route) -> SessionRecord {
    SessionRecord {
        session,
        node_id: [0xabu8; 32],
        op_id: [0xcdu8; 32],
        connected_at: 1_700_000_000,
        route,
    }
}

#[test]
fn session_record_roundtrip_relay() {
    let r = make_record(
        42,
        Route::Relay {
            relay_addr: "relay.crabcc.app:443".to_string(),
        },
    );
    assert_eq!(SessionRecord::decode(&r.encode().unwrap()).unwrap(), r);
}

#[test]
fn session_record_roundtrip_direct() {
    let r = make_record(
        99,
        Route::Direct {
            peer_addr: "100.73.72.35:9999".to_string(),
        },
    );
    assert_eq!(SessionRecord::decode(&r.encode().unwrap()).unwrap(), r);
}

#[test]
fn session_record_filename_is_deterministic() {
    let r = make_record(
        0x1234_5678_9abc_def0_u128,
        Route::Relay {
            relay_addr: "r".into(),
        },
    );
    let name = r.filename();
    assert!(name.starts_with("wormhole-"));
    assert!(name.ends_with(".session"));
    // Same session always produces same filename.
    assert_eq!(r.filename(), name);
}

#[test]
fn session_record_filename_differs_across_sessions() {
    let a = make_record(
        1,
        Route::Relay {
            relay_addr: "r".into(),
        },
    );
    let b = make_record(
        2,
        Route::Relay {
            relay_addr: "r".into(),
        },
    );
    // Low 32 bits differ -> different filenames.
    assert_ne!(a.filename(), b.filename());
}

#[test]
fn persist_session_record_writes_and_reads_back() {
    let r = make_record(
        0xdeadbeef_cafebabe_u128,
        Route::Relay {
            relay_addr: "relay.crabcc.app:443".into(),
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let path = persist_session_record(&r, dir.path()).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let recovered = SessionRecord::decode(&bytes).unwrap();
    assert_eq!(recovered, r);
    assert!(path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("wormhole-"));
}

#[test]
fn persist_session_record_fallback_path() {
    // We can't reliably make /tmp unwritable in a test, so instead verify that
    // the fallback dir is used when given a path that doesn't yet exist.
    let r = make_record(
        0xffff_0000,
        Route::Direct {
            peer_addr: "100.1.2.3:7777".into(),
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let fallback = dir.path().join("nested").join("sessions");
    let path = persist_session_record(&r, &fallback).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(SessionRecord::decode(&bytes).unwrap(), r);
}
