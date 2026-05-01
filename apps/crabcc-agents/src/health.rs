use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{debug, warn};

use crate::runner::Runner;
use crate::streams::LogStreamer;

/// Tiny `/healthz` HTTP server. No external HTTP framework — one
/// endpoint, hand-parsed request line, fixed response.
///
/// Reports `200 OK` only when both Docker and Redis are reachable.
pub async fn serve(addr: SocketAddr, runner: Arc<Runner>, streamer: Arc<LogStreamer>) {
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            warn!(%e, %addr, "health listener bind failed — disabled");
            return;
        }
    };
    debug!(%addr, "health listener up");
    loop {
        let (mut sock, _peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                warn!(%e, "accept");
                continue;
            }
        };
        let runner = runner.clone();
        let streamer = streamer.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 256];
            let _ = sock.read(&mut buf).await;
            let body_ok = b"ok\n";
            let body_bad = b"unhealthy\n";

            let healthy = runner.ping().await.is_ok() && streamer.ping().await.is_ok();
            let (status, body) = if healthy {
                ("200 OK", &body_ok[..])
            } else {
                ("503 Service Unavailable", &body_bad[..])
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.write_all(body).await;
            let _ = sock.shutdown().await;
        });
    }
}
