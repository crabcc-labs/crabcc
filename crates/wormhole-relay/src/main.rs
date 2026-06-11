// Wave 1 Task 2: WS accept, nonce auth, route by node_id,
// append-only blob log + offset replay, caps + GapNotice.
// See docs/WORMHOLE.md §11.
//
// Wave 2: SessionStore trait + generic handlers, WsSession<Phase> typestate
// (Unauthenticated → Registered). Relay blindness invariant preserved: this
// file never calls decrypt, snow::, or touches Envelope internals.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::Mutex;
use tracing::{info, warn};
use wormhole_proto::{Envelope, Kind, OuterFrame, ReplayLog};

// ---- constants ----

const REPLAY_CAP: usize = 8 * 1024 * 1024; // 8 MB per node_id blob log
const NONCE_TTL: Duration = Duration::from_secs(30);
const CHAN_BUF: usize = 64;

// ---- SessionStore trait ----

/// Result of a replay request: an optional `(from, to)` gap notice plus the
/// `(seq, payload)` frames to resend.
type ReplayResult = (Option<(u64, u64)>, Vec<(u64, Vec<u8>)>);

/// Abstracts the relay's mutable state so tests and alternative backends can
/// plug in without modifying handler logic. Callers hold a `Mutex<S>` lock.
trait SessionStore: Send + 'static {
    /// Evict stale nonces, then check `nonce` for replay. Returns `true` if
    /// the nonce was fresh and has been recorded; `false` = replay attack.
    fn check_and_insert_nonce(&mut self, nonce: [u8; 16]) -> bool;

    /// Register the mpsc sender for (node_id, role=0 node / role=1 operator).
    fn register_sender(&mut self, node_id: [u8; 32], role: u8, tx: Sender<Message>);

    /// Log `payload` for role=0 (node→relay frames only) and return the peer
    /// sender. Returns `None` when the peer channel is not connected.
    fn route_frame(
        &mut self,
        node_id: [u8; 32],
        role: u8,
        payload: Vec<u8>,
    ) -> Option<Sender<Message>>;

    /// Remove the sender for (node_id, role) on disconnect.
    fn deregister_sender(&mut self, node_id: [u8; 32], role: u8);

    /// Bearer token for replay endpoint auth; `None` = no auth required.
    fn relay_token(&self) -> Option<&str>;

    /// Return `(gap, frames)` for a node_id from `from_seq`, or `None` if the
    /// node is unknown. Returned data is owned (avoids borrow-across-await).
    fn replay(&self, node_id: [u8; 32], from_seq: u64) -> Option<ReplayResult>;

    /// Evict nonces older than NONCE_TTL (called by background task).
    fn evict_nonces(&mut self);
}

// ---- concrete state ----

#[derive(Default)]
struct RelayState {
    sessions: HashMap<[u8; 32], SessionState>,
    nonces: HashMap<[u8; 16], Instant>,
    relay_token: Option<String>,
}

struct SessionState {
    node_tx: Option<Sender<Message>>,
    op_tx: Option<Sender<Message>>,
    log: ReplayLog,
    node_seq: u64,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            node_tx: None,
            op_tx: None,
            log: ReplayLog::new(REPLAY_CAP),
            node_seq: 0,
        }
    }
}

impl SessionStore for RelayState {
    fn check_and_insert_nonce(&mut self, nonce: [u8; 16]) -> bool {
        self.evict_nonces();
        if self.nonces.contains_key(&nonce) {
            return false;
        }
        self.nonces.insert(nonce, Instant::now());
        true
    }

    fn register_sender(&mut self, node_id: [u8; 32], role: u8, tx: Sender<Message>) {
        let sess = self.sessions.entry(node_id).or_default();
        match role {
            0 => sess.node_tx = Some(tx),
            1 => sess.op_tx = Some(tx),
            _ => unreachable!(),
        }
    }

    fn route_frame(
        &mut self,
        node_id: [u8; 32],
        role: u8,
        payload: Vec<u8>,
    ) -> Option<Sender<Message>> {
        let sess = self.sessions.entry(node_id).or_default();
        match role {
            0 => {
                // node → relay: log the payload, then forward to operator.
                let seq = sess.node_seq;
                sess.node_seq += 1;
                sess.log.push(seq, payload);
                sess.op_tx.clone()
            }
            1 => sess.node_tx.clone(), // operator → relay: forward to node.
            _ => unreachable!(),
        }
    }

    fn deregister_sender(&mut self, node_id: [u8; 32], role: u8) {
        if let Some(sess) = self.sessions.get_mut(&node_id) {
            match role {
                0 => sess.node_tx = None,
                1 => sess.op_tx = None,
                _ => {}
            }
        }
    }

    fn relay_token(&self) -> Option<&str> {
        self.relay_token.as_deref()
    }

    fn replay(&self, node_id: [u8; 32], from_seq: u64) -> Option<ReplayResult> {
        let sess = self.sessions.get(&node_id)?;
        let gap = sess.log.gap().filter(|g| from_seq <= g.1);
        let frames = sess
            .log
            .replay_from(from_seq)
            .map(|(seq, payload)| (*seq, payload.clone()))
            .collect();
        Some((gap, frames))
    }

    fn evict_nonces(&mut self) {
        self.nonces.retain(|_, t| t.elapsed() < NONCE_TTL);
    }
}

// ---- typestate: WS handshake phases ----

struct Unauthenticated;

struct Registered {
    node_id: [u8; 32],
    role: u8,
}

/// Zero-cost typestate wrapper for the per-connection WS handshake lifecycle.
/// `Phase` carries phase-specific data; the type parameter prevents using
/// `node_id`/`role` before nonce verification succeeds.
struct WsSession<Phase>(Phase);

impl WsSession<Unauthenticated> {
    fn new() -> Self {
        WsSession(Unauthenticated)
    }

    /// Parse `raw` handshake bytes, verify the nonce, register the outbound
    /// sender, and advance to `Registered`. Returns `None` on any failure.
    async fn authenticate<S: SessionStore>(
        self,
        raw: &[u8],
        state: &Arc<Mutex<S>>,
        tx: Sender<Message>,
    ) -> Option<WsSession<Registered>> {
        if raw.len() < 49 {
            warn!("handshake too short ({}B)", raw.len());
            return None;
        }
        let mut nonce = [0u8; 16];
        nonce.copy_from_slice(&raw[..16]);
        let mut node_id = [0u8; 32];
        node_id.copy_from_slice(&raw[16..48]);
        let role = raw[48]; // 0 = node, 1 = operator
        if role > 1 {
            warn!("unknown role byte {role}");
            return None;
        }

        let mut st = state.lock().await;
        if !st.check_and_insert_nonce(nonce) {
            warn!("replay nonce rejected");
            return None;
        }
        st.register_sender(node_id, role, tx);

        Some(WsSession(Registered { node_id, role }))
    }
}

impl WsSession<Registered> {
    fn node_id(&self) -> [u8; 32] {
        self.0.node_id
    }
    fn role(&self) -> u8 {
        self.0.role
    }
}

// ---- router ----

fn build_router<S>(state: Arc<Mutex<S>>) -> Router
where
    S: SessionStore + 'static,
{
    Router::new()
        .route("/wormhole/v1", get(ws_handler::<S>))
        .route("/wormhole/v1/replay/:node_id", get(replay_handler::<S>))
        .route("/wormhole/v1/health", get(health_handler))
        .with_state(state)
}

// ---- health ----

async fn health_handler() -> impl IntoResponse {
    r#"{"status":"ok"}"#
}

// ---- WebSocket handler ----

async fn ws_handler<S>(ws: WebSocketUpgrade, State(state): State<Arc<Mutex<S>>>) -> Response
where
    S: SessionStore + 'static,
{
    ws.on_upgrade(move |socket| handle_ws::<S>(socket, state))
}

async fn handle_ws<S: SessionStore>(socket: WebSocket, state: Arc<Mutex<S>>) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // First message: [16-byte nonce] ++ [32-byte node_id] ++ [1-byte role]
    let handshake_bytes = match ws_rx.next().await {
        Some(Ok(Message::Binary(b))) => b,
        _ => return,
    };

    let (tx, mut rx) = mpsc::channel::<Message>(CHAN_BUF);

    let session = match WsSession::new()
        .authenticate(&handshake_bytes, &state, tx)
        .await
    {
        Some(s) => s,
        None => return,
    };

    // Outbound pump: drains the mpsc rx into the WS sink.
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Receive loop: parse OuterFrame and route to peer.
    loop {
        let bytes = match ws_rx.next().await {
            Some(Ok(Message::Binary(b))) => b,
            Some(Ok(Message::Close(_))) | None => break,
            _ => continue,
        };

        let frame = match OuterFrame::decode(&bytes) {
            Ok(f) => f,
            Err(e) => {
                warn!("OuterFrame decode error: {e}");
                continue;
            }
        };

        // Clone peer sender before dropping lock so try_send runs outside it.
        let peer_tx = state.lock().await.route_frame(
            session.node_id(),
            session.role(),
            frame.noise_payload.clone(),
        );

        if let Some(tx) = peer_tx {
            let _ = tx.try_send(Message::Binary(bytes));
        }
    }

    // Disconnect: clear this role's sender so peer stops receiving.
    state
        .lock()
        .await
        .deregister_sender(session.node_id(), session.role());

    send_task.abort();
}

// ---- replay endpoint ----

#[derive(Deserialize)]
struct ReplayQuery {
    from: Option<u64>,
}

/// Decode a lowercase hex string to exactly `N` bytes.
fn decode_hex_exact<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)? as u8;
        let lo = (chunk[1] as char).to_digit(16)? as u8;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

/// Encode bytes as lowercase hex.
fn encode_hex(b: &[u8]) -> String {
    b.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn replay_handler<S: SessionStore>(
    headers: HeaderMap,
    Path(node_id_hex): Path<String>,
    Query(params): Query<ReplayQuery>,
    State(state): State<Arc<Mutex<S>>>,
) -> impl IntoResponse {
    let Some(node_id) = decode_hex_exact::<32>(&node_id_hex) else {
        return (StatusCode::BAD_REQUEST, "invalid node_id\n".to_string());
    };

    let from_seq = params.from.unwrap_or(0);

    // Auth check + data fetch under one lock acquisition.
    let result = {
        let st = state.lock().await;
        if let Some(tok) = st.relay_token() {
            let provided = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if provided != format!("Bearer {tok}") {
                return (StatusCode::UNAUTHORIZED, "unauthorized\n".to_string());
            }
        }
        st.replay(node_id, from_seq)
    };

    let Some((gap, frames)) = result else {
        return (StatusCode::NOT_FOUND, "unknown node\n".to_string());
    };

    let mut lines: Vec<String> = Vec::new();

    // Prepend a GapNotice frame when the log has a gap overlapping the request.
    if let Some((gap_from, gap_to)) = gap {
        let notice_env = Envelope {
            session: 0,
            seq: 0,
            kind: Kind::GapNotice {
                from: gap_from,
                to: gap_to,
            },
            body: vec![],
        };
        if let Ok(env_bytes) = notice_env.encode() {
            let notice_frame = OuterFrame {
                node_id,
                channel: 0,
                noise_payload: env_bytes,
            };
            if let Ok(encoded) = notice_frame.encode() {
                lines.push(encode_hex(&encoded));
            }
        }
    }

    for (_seq, payload) in &frames {
        lines.push(encode_hex(payload));
    }

    (StatusCode::OK, lines.join("\n"))
}

// ---- main ----

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let port: u16 = std::env::var("WORMHOLE_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4443);
    let relay_token = std::env::var("RELAY_TOKEN").ok();
    let state: Arc<Mutex<RelayState>> = Arc::new(Mutex::new(RelayState {
        relay_token,
        ..Default::default()
    }));

    // Background task: evict stale nonces every NONCE_TTL so bursts of
    // short-lived connections don't accumulate them indefinitely.
    tokio::spawn({
        let state = Arc::clone(&state);
        async move {
            let mut interval = tokio::time::interval(NONCE_TTL);
            loop {
                interval.tick().await;
                state.lock().await.evict_nonces();
            }
        }
    });

    let app = build_router(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("wormhole-relay listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
