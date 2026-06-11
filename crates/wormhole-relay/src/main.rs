// Wave 1 Task 2: WS accept, nonce auth, route by node_id,
// append-only blob log + offset replay, caps + GapNotice.
// See docs/WORMHOLE.md §11.
//
// Relay blindness invariant: this file never calls decrypt, snow::, or
// touches Envelope internals. Only OuterFrame is deserialized.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
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

// ---- state ----

#[derive(Default)]
struct RelayState {
    sessions: HashMap<[u8; 32], SessionState>,
    nonces: HashMap<[u8; 16], Instant>,
}

struct SessionState {
    node_tx: Option<Sender<Message>>,
    op_tx: Option<Sender<Message>>,
    log: ReplayLog,
    node_seq: u64, // incremented for each node->relay frame pushed to log
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

type SharedState = Arc<Mutex<RelayState>>;

// ---- router ----

fn build_router(state: SharedState) -> Router {
    Router::new()
        .route("/wormhole/v1", get(ws_handler))
        .route("/wormhole/v1/replay/:node_id", get(replay_handler))
        .route("/wormhole/v1/health", get(health_handler))
        .with_state(state)
}

// ---- health ----

async fn health_handler() -> impl IntoResponse {
    r#"{"status":"ok"}"#
}

// ---- nonce helpers ----

/// Sweep nonces older than NONCE_TTL. Called on each new connection so no
/// background task is needed.
fn evict_old_nonces(nonces: &mut HashMap<[u8; 16], Instant>) {
    nonces.retain(|_, seen_at| seen_at.elapsed() < NONCE_TTL);
}

// ---- WebSocket handler ----

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<SharedState>) -> Response {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: SharedState) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // First message: [16-byte nonce] ++ [32-byte node_id] ++ [1-byte role]
    let handshake = match ws_rx.next().await {
        Some(Ok(Message::Binary(b))) => b,
        _ => return,
    };

    if handshake.len() < 49 {
        warn!("handshake too short ({}B)", handshake.len());
        return;
    }

    let mut nonce = [0u8; 16];
    nonce.copy_from_slice(&handshake[..16]);
    let mut node_id = [0u8; 32];
    node_id.copy_from_slice(&handshake[16..48]);
    let role = handshake[48]; // 0 = node, 1 = operator

    if role > 1 {
        warn!("unknown role byte {role}");
        return;
    }

    // Nonce check
    {
        let mut st = state.lock().await;
        evict_old_nonces(&mut st.nonces);
        if st.nonces.contains_key(&nonce) {
            warn!("replay nonce rejected");
            return;
        }
        st.nonces.insert(nonce, Instant::now());
    }

    // Register sender channel for this role
    let (tx, mut rx) = mpsc::channel::<Message>(CHAN_BUF);
    {
        let mut st = state.lock().await;
        let sess = st.sessions.entry(node_id).or_default();
        match role {
            0 => sess.node_tx = Some(tx),
            1 => sess.op_tx = Some(tx),
            _ => unreachable!(),
        }
    }

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
        let msg = match ws_rx.next().await {
            Some(Ok(m)) => m,
            _ => break,
        };

        let bytes = match msg {
            Message::Binary(b) => b,
            Message::Close(_) => break,
            _ => continue,
        };

        let frame = match OuterFrame::decode(&bytes) {
            Ok(f) => f,
            Err(e) => {
                warn!("OuterFrame decode error: {e}");
                continue;
            }
        };

        let mut st = state.lock().await;
        let sess = st.sessions.entry(node_id).or_default();

        match role {
            0 => {
                // node -> relay: log the payload, forward raw bytes to operator
                let seq = sess.node_seq;
                sess.node_seq += 1;
                sess.log.push(seq, frame.noise_payload.clone());
                if let Some(op_tx) = &sess.op_tx {
                    let _ = op_tx.try_send(Message::Binary(bytes));
                }
            }
            1 => {
                // operator -> relay: forward raw bytes to node
                if let Some(node_tx) = &sess.node_tx {
                    let _ = node_tx.try_send(Message::Binary(bytes));
                }
            }
            _ => unreachable!(),
        }
    }

    // Disconnect: clear this role's sender so peer stops receiving.
    {
        let mut st = state.lock().await;
        if let Some(sess) = st.sessions.get_mut(&node_id) {
            match role {
                0 => sess.node_tx = None,
                1 => sess.op_tx = None,
                _ => {}
            }
        }
    }

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

async fn replay_handler(
    Path(node_id_hex): Path<String>,
    Query(params): Query<ReplayQuery>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    let Some(node_id) = decode_hex_exact::<32>(&node_id_hex) else {
        return (axum::http::StatusCode::BAD_REQUEST, "invalid node_id\n".to_string());
    };

    let from_seq = params.from.unwrap_or(0);

    let st = state.lock().await;
    let Some(sess) = st.sessions.get(&node_id) else {
        return (axum::http::StatusCode::NOT_FOUND, "unknown node\n".to_string());
    };

    let mut lines: Vec<String> = Vec::new();

    // Prepend a GapNotice frame when the log has a gap overlapping the request.
    if let Some(g) = sess.log.gap() {
        if from_seq <= g.1 {
            let notice_env = Envelope {
                session: 0,
                seq: 0,
                kind: Kind::GapNotice { from: g.0, to: g.1 },
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
    }

    for (_seq, payload) in sess.log.replay_from(from_seq) {
        lines.push(encode_hex(payload));
    }

    (axum::http::StatusCode::OK, lines.join("\n"))
}

// ---- main ----

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let port: u16 = std::env::var("WORMHOLE_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4443);
    let state: SharedState = Arc::new(Mutex::new(RelayState::default()));
    let app = build_router(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("wormhole-relay listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
