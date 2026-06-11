//! Relay connection and Noise_IK transport loop.
//!
//! Hot path: recv WS → Noise decrypt → Envelope decode → dispatch → Envelope encode
//! → Noise encrypt → send WS.  Two `Box<[u8; NOISE_BUF]>` are allocated once per
//! session and reused across every frame, eliminating ~130 KB of heap pressure per
//! round-trip.  Slice length invariants are upheld by snow's contract (returns `len
//! <= out.len()`) and validated once with debug_assert; production builds use
//! `unsafe get_unchecked` to skip the bounds check entirely.

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};
use wormhole_proto::{
    persist_session_record, Envelope, Kind, OuterFrame, Route, SeqState, SessionRecord,
};

use crate::cmd::{dispatch, NodeCmd, NodeEvent};
use crate::keys::NodeKeys;

const NOISE_PARAMS: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
// ChaChaPoly adds 16-byte tag; postcard envelope is at most MAX_BODY_BYTES + overhead.
const NOISE_BUF: usize = 65536 + 128;

type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsRx = futures_util::stream::SplitStream<WsStream>;
type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Serialize, Deserialize, Default)]
struct SeqWatermark {
    inbound_next: u64,
    outbound_next: u64,
}

/// Per-session pre-allocated noise I/O scratch space.
/// Kept on the heap (Box) so it doesn't blow the async-task stack.
struct NoiseBufs {
    recv: Box<[u8; NOISE_BUF]>,
    send: Box<[u8; NOISE_BUF]>,
}

impl NoiseBufs {
    #[inline(always)]
    fn new() -> Self {
        Self {
            recv: Box::new([0u8; NOISE_BUF]),
            send: Box::new([0u8; NOISE_BUF]),
        }
    }
}

pub async fn run_session(
    relay_url: &str,
    keys: &NodeKeys,
    op_static_pub: Option<[u8; 32]>,
    wormhole_dir: &Path,
) -> Result<()> {
    let (ws, _) = tokio_tungstenite::connect_async(relay_url)
        .await
        .context("connect to relay")?;
    info!("connected to relay");

    let (mut sink, mut rx) = ws.split();

    // Auth handshake: [16B nonce][32B node_id][1B role=0]
    let mut auth = [0u8; 49];
    getrandom::getrandom(&mut auth[..16]).context("getrandom nonce")?;
    auth[16..48].copy_from_slice(&keys.node_id);
    auth[48] = 0; // role = node
    sink.send(Message::Binary(auth.to_vec().into()))
        .await
        .context("send auth")?;
    info!(node_id = hex(&keys.node_id), "sent auth");

    // Noise IK handshake — happens once per relay connection.
    let mut bufs = NoiseBufs::new();
    let mut transport =
        noise_handshake(&mut rx, &mut sink, keys, op_static_pub, &mut bufs).await?;
    info!("noise handshake complete");

    let op_id: [u8; 32] = transport
        .get_remote_static()
        .map(|s| blake3::hash(s).into())
        .ok_or_else(|| anyhow!("no remote static after handshake"))?;
    info!(op_id = hex(&op_id), "operator identity");

    let session_id: u128 = {
        let mut b = [0u8; 16];
        getrandom::getrandom(&mut b).context("getrandom session_id")?;
        u128::from_le_bytes(b)
    };

    let mut seq = SeqState::new();
    let mut _token: Option<Vec<u8>> = None; // stored; verified in Wave 4 (biscuit-auth)
    let mut hello_done = false;

    loop {
        let outer = match recv_frame(&mut rx).await {
            Ok(f) => f,
            Err(e) => {
                warn!("recv: {e}");
                break;
            }
        };

        // Noise decrypt — reuse recv buf, skip bounds check (snow upholds len ≤ buf.len())
        let plain_len =
            match transport.read_message(&outer.noise_payload, bufs.recv.as_mut_slice()) {
                Ok(n) => n,
                Err(e) => {
                    warn!("noise decrypt: {e}");
                    continue;
                }
            };
        debug_assert!(plain_len <= NOISE_BUF);
        // SAFETY: snow guarantees plain_len <= bufs.recv.len()
        let plain = unsafe { bufs.recv.get_unchecked(..plain_len) };

        let env = match Envelope::decode(plain) {
            Ok(e) => e,
            Err(e) => {
                warn!("envelope decode: {e}");
                continue;
            }
        };

        if !seq.accept(env.seq) {
            warn!(seq = env.seq, "seq rejected");
            continue;
        }

        match env.kind.clone() {
            Kind::Hello => {
                if !hello_done {
                    hello_done = true;
                    let rec = SessionRecord {
                        session: session_id,
                        node_id: keys.node_id,
                        op_id,
                        connected_at: unix_now(),
                        route: Route::Relay { relay_addr: String::new() },
                    };
                    if let Err(e) =
                        persist_session_record(&rec, &wormhole_dir.join("sessions"))
                    {
                        warn!("persist session record: {e}");
                    }
                }
                send_envelope(
                    &mut sink,
                    &mut transport,
                    &mut bufs,
                    &keys.node_id,
                    session_id,
                    &mut seq,
                    Kind::Hello,
                    &[],
                )
                .await?;
                save_seq(wormhole_dir, session_id, &seq)?;
            }

            Kind::Cmd => {
                let event = match postcard::from_bytes::<NodeCmd>(&env.body) {
                    Ok(cmd) => dispatch(cmd, &keys.node_id).await,
                    Err(e) => NodeEvent::Error { msg: e.to_string() },
                };
                let body = postcard::to_allocvec(&event).context("encode event")?;
                send_envelope(
                    &mut sink,
                    &mut transport,
                    &mut bufs,
                    &keys.node_id,
                    session_id,
                    &mut seq,
                    Kind::Event,
                    &body,
                )
                .await?;
                save_seq(wormhole_dir, session_id, &seq)?;
            }

            Kind::Ping => {
                send_envelope(
                    &mut sink,
                    &mut transport,
                    &mut bufs,
                    &keys.node_id,
                    session_id,
                    &mut seq,
                    Kind::Pong,
                    &[],
                )
                .await?;
            }

            Kind::Resume { from_seq } => {
                seq.advance_inbound_to(from_seq);
            }

            Kind::TokenRefresh { token: new_token } => {
                _token = Some(new_token);
                send_envelope(
                    &mut sink,
                    &mut transport,
                    &mut bufs,
                    &keys.node_id,
                    session_id,
                    &mut seq,
                    Kind::TokenAck { expires_at: 0 },
                    &[],
                )
                .await?;
            }

            other => warn!(kind = ?other, "unhandled kind"),
        }
    }

    Ok(())
}

// ── Noise IK handshake ─────────────────────────────────────────────────────

async fn noise_handshake(
    rx: &mut WsRx,
    sink: &mut WsSink,
    keys: &NodeKeys,
    op_static_pub: Option<[u8; 32]>,
    bufs: &mut NoiseBufs,
) -> Result<snow::TransportState> {
    let params: snow::params::NoiseParams =
        NOISE_PARAMS.parse().context("parse noise params")?;
    let mut builder =
        snow::Builder::new(params).local_private_key(&keys.static_secret);
    if let Some(ref op_pub) = op_static_pub {
        builder = builder.remote_public_key(op_pub.as_ref());
    }
    let mut hs = builder.build_responder().context("build noise responder")?;

    // msg1: read from operator
    let msg1 = recv_frame(rx).await?;
    hs.read_message(&msg1.noise_payload, bufs.recv.as_mut_slice())
        .context("noise read msg1")?;

    // msg2: write back
    let msg2_len = hs
        .write_message(&[], bufs.send.as_mut_slice())
        .context("noise write msg2")?;
    debug_assert!(msg2_len <= NOISE_BUF);
    // SAFETY: snow guarantees msg2_len <= bufs.send.len()
    let msg2_payload = unsafe { bufs.send.get_unchecked(..msg2_len) }.to_vec();
    send_raw(
        sink,
        &OuterFrame { node_id: keys.node_id, channel: 0, noise_payload: msg2_payload },
    )
    .await?;

    hs.into_transport_mode().context("noise transport mode")
}

// ── frame I/O ──────────────────────────────────────────────────────────────

#[inline(always)]
async fn recv_frame(rx: &mut WsRx) -> Result<OuterFrame> {
    loop {
        match rx.next().await {
            Some(Ok(Message::Binary(b))) => {
                return OuterFrame::decode(&b).context("decode outer frame");
            }
            Some(Ok(Message::Close(_))) => bail!("websocket closed"),
            Some(Ok(_)) => continue,
            Some(Err(e)) => bail!("ws recv: {e}"),
            None => bail!("ws stream ended"),
        }
    }
}

#[inline(always)]
async fn send_raw(sink: &mut WsSink, frame: &OuterFrame) -> Result<()> {
    let bytes = frame.encode().context("encode outer frame")?;
    sink.send(Message::Binary(bytes.into()))
        .await
        .context("send frame")
}

#[inline(always)]
async fn send_envelope(
    sink: &mut WsSink,
    transport: &mut snow::TransportState,
    bufs: &mut NoiseBufs,
    node_id: &[u8; 32],
    session: u128,
    seq: &mut SeqState,
    kind: Kind,
    body: &[u8],
) -> Result<()> {
    let env = Envelope {
        session,
        seq: seq.next_send(),
        kind,
        body: body.to_vec(),
    };
    let plain = env.encode().context("encode envelope")?;
    let cipher_len = transport
        .write_message(&plain, bufs.send.as_mut_slice())
        .context("noise encrypt")?;
    debug_assert!(cipher_len <= NOISE_BUF);
    // SAFETY: snow guarantees cipher_len <= bufs.send.len()
    let noise_payload = unsafe { bufs.send.get_unchecked(..cipher_len) }.to_vec();
    send_raw(
        sink,
        &OuterFrame { node_id: *node_id, channel: 0, noise_payload },
    )
    .await
}

// ── persistence ────────────────────────────────────────────────────────────

fn save_seq(wormhole_dir: &Path, session: u128, seq: &SeqState) -> Result<()> {
    let hex8 = format!("{:08x}", (session & 0xffff_ffff) as u32);
    let path = wormhole_dir.join(format!("{hex8}.seq"));
    let inbound_next = seq.watermark().map(|w| w + 1).unwrap_or(0);
    let wm = SeqWatermark { inbound_next, outbound_next: 0 };
    let bytes = postcard::to_allocvec(&wm).context("encode seq watermark")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, &bytes).context("write seq watermark")
}

// ── helpers ────────────────────────────────────────────────────────────────

#[inline(always)]
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

#[inline(always)]
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}
