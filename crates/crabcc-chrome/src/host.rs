//! Native-messaging host mode. Chrome launches one of these per
//! `chrome.runtime.connectNative` call. We bridge the Chrome side
//! (framed JSON on stdin/stdout) to the long-lived `serve` process via
//! a TCP loopback connection; line-delimited JSON on the wire.
//!
//! Lifecycle:
//! 1. Read shared-secret + port from `~/.crabcc/chrome.toml`.
//! 2. Connect to `127.0.0.1:<port>`. Auth handshake: send
//!    `{"kind":"auth","secret":"<hex>"}` as a single JSON line.
//! 3. Spawn one thread per direction — Chrome→bridge and bridge→Chrome
//!    — each translating between the framing dialects.
//! 4. Exit when either direction sees EOF; Chrome will tear the
//!    process down on its own anyway when the extension disconnects.

use anyhow::{anyhow, Context, Result};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::config;
use crate::framing;

const CONNECT_RETRY: u32 = 5;
const CONNECT_BACKOFF_MS: u64 = 200;

pub fn run() -> Result<()> {
    let cfg = config::load_or_default();
    if cfg.port == 0 {
        return Err(anyhow!(
            "no `port` in chrome.toml — run `crabcc-chrome serve` before connecting"
        ));
    }
    if cfg.secret.is_empty() {
        return Err(anyhow!(
            "no `secret` in chrome.toml — run `crabcc-chrome pair --id <ext-id>` first"
        ));
    }

    let stream = connect_with_retry(cfg.port)?;
    tracing::info!(port = cfg.port, "host: connected to bridge");

    // Auth handshake. The server reads exactly one line and validates
    // before forwarding any traffic in either direction.
    let mut writer = stream.try_clone().context("cloning stream for write")?;
    let auth = serde_json::json!({
        "kind": "auth",
        "secret": cfg.secret,
        "wireVersion": crate::WIRE_VERSION,
    });
    writeln!(writer, "{}", auth).context("sending auth")?;
    writer.flush().ok();

    let reader = BufReader::new(stream);

    // Chrome → bridge: read framed messages from stdin, write each as a
    // single JSON line to the bridge socket.
    let writer_clone = writer
        .try_clone()
        .context("cloning writer for chrome→bridge")?;
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let done_tx_a = done_tx.clone();
    let chrome_to_bridge = thread::spawn(move || {
        let mut w = writer_clone;
        let stdin = io::stdin();
        let mut stdin = stdin.lock();
        loop {
            match framing::read_frame(&mut stdin) {
                Ok(Some(body)) => {
                    if w.write_all(&body).is_err() || w.write_all(b"\n").is_err() {
                        break;
                    }
                    w.flush().ok();
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "chrome→bridge: read error");
                    break;
                }
            }
        }
        let _ = done_tx_a.send(());
    });

    // Bridge → Chrome: read newline-delimited JSON from socket, frame
    // each as a native-messaging message on stdout.
    let bridge_to_chrome = thread::spawn(move || {
        let mut r = reader;
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        let mut line = String::new();
        loop {
            line.clear();
            match r.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
                    if trimmed.is_empty() {
                        continue;
                    }
                    if framing::write_frame(&mut stdout, trimmed.as_bytes()).is_err() {
                        break;
                    }
                    if stdout.flush().is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "bridge→chrome: read error");
                    break;
                }
            }
        }
        let _ = done_tx.send(());
    });

    // Block until either direction closes; let the OS reap the other
    // thread when we exit. Chrome will close stdin on disconnect, so the
    // chrome-to-bridge thread exits cleanly; the bridge thread may
    // linger on a half-closed socket until our process death.
    let _ = done_rx.recv();
    drop(chrome_to_bridge);
    drop(bridge_to_chrome);
    Ok(())
}

fn connect_with_retry(port: u16) -> Result<TcpStream> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..CONNECT_RETRY {
        match TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
            Ok(s) => {
                s.set_nodelay(true).ok();
                return Ok(s);
            }
            Err(e) => {
                last_err = Some(e);
                let backoff = CONNECT_BACKOFF_MS * (1 << attempt);
                thread::sleep(Duration::from_millis(backoff));
            }
        }
    }
    Err(anyhow!(
        "could not connect to bridge on 127.0.0.1:{port}: {}",
        last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown".into())
    ))
}
