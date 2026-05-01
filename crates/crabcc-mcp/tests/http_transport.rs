// Integration tests for the HTTP transport (#204 phase 1).
//
// Approach: spawn `serve_http` in a background thread bound to an
// ephemeral port, then drive raw HTTP/1.1 requests over a TcpStream
// from the test thread. Avoids pulling reqwest into dev-deps just
// for a couple of POSTs.
//
// The server thread leaks on test exit — that's fine, the process
// goes away with all its threads. tiny_http's Server has no graceful
// shutdown without an explicit channel, and adding one is more
// machinery than the test value justifies.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

/// Pick an ephemeral port by binding 0, then closing the listener so
/// the OS doesn't reuse it for ~30s (TIME_WAIT). Race window is
/// tolerable for tests; if a parallel test grabs the same port the
/// HTTP request will simply 404 and we retry.
fn ephemeral_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind 0");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// Spawn the HTTP transport in a background thread. Returns the
/// bound socket addr so the test can target it directly.
fn spawn_server(token: Option<String>) -> (SocketAddr, PathBuf, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let port = ephemeral_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let root_clone = root.clone();
    thread::spawn(move || {
        let _ = crabcc_mcp::serve_http(addr, &root_clone, false, token);
    });
    // Give the server thread a moment to bind. tiny_http binds
    // synchronously inside Server::http but we race the test if we
    // skip this — 50 ms is enough on every box we've measured.
    thread::sleep(Duration::from_millis(50));
    (addr, root, tmp)
}

/// Send a raw HTTP/1.1 request and return (status_code, body).
/// Bare-minimum parser; assumes server returns a non-chunked body
/// and a Content-Length we can trust (tiny_http does both).
fn http_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    req.push_str("Connection: close\r\n\r\n");
    req.push_str(body);

    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    stream.write_all(req.as_bytes()).expect("write");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read");

    let mut parts = raw.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("").to_string();
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, body)
}

#[test]
fn health_endpoint_returns_ok() {
    let (addr, _root, _tmp) = spawn_server(None);
    let (status, body) = http_request(addr, "GET", "/health", &[], "");
    assert_eq!(status, 200, "GET /health expected 200, got {status}: {body}");
    assert!(body.contains("\"status\":\"ok\""), "body: {body}");
    assert!(body.contains("\"transport\":\"http\""), "body: {body}");
}

#[test]
fn unknown_path_returns_404() {
    let (addr, _root, _tmp) = spawn_server(None);
    let (status, _body) = http_request(addr, "GET", "/nope", &[], "");
    assert_eq!(status, 404);
}

#[test]
fn post_mcp_with_invalid_json_returns_parse_error() {
    let (addr, _root, _tmp) = spawn_server(None);
    let (status, body) = http_request(
        addr,
        "POST",
        "/mcp",
        &[("Content-Type", "application/json")],
        "not json",
    );
    assert_eq!(status, 200, "parse-error JSON-RPC reply rides on 200");
    assert!(body.contains("-32700"), "expected parse-error code -32700: {body}");
}

#[test]
fn post_mcp_initialize_round_trips() {
    let (addr, _root, _tmp) = spawn_server(None);
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let (status, resp) = http_request(
        addr,
        "POST",
        "/mcp",
        &[("Content-Type", "application/json")],
        body,
    );
    assert_eq!(status, 200, "initialize expected 200, got {status}: {resp}");
    assert!(resp.contains("\"jsonrpc\":\"2.0\""), "missing jsonrpc tag: {resp}");
    assert!(resp.contains("\"id\":1"), "missing id echo: {resp}");
}

#[test]
fn post_mcp_without_token_returns_401_when_auth_required() {
    let (addr, _root, _tmp) = spawn_server(Some("expected-secret".to_string()));
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let (status, _resp) = http_request(
        addr,
        "POST",
        "/mcp",
        &[("Content-Type", "application/json")],
        body,
    );
    assert_eq!(status, 401);
}

#[test]
fn post_mcp_with_correct_token_succeeds() {
    let (addr, _root, _tmp) = spawn_server(Some("expected-secret".to_string()));
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let (status, resp) = http_request(
        addr,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Authorization", "Bearer expected-secret"),
        ],
        body,
    );
    assert_eq!(status, 200, "got {status}: {resp}");
    assert!(resp.contains("\"id\":1"), "missing id echo: {resp}");
}

#[test]
fn post_mcp_with_wrong_token_returns_401() {
    let (addr, _root, _tmp) = spawn_server(Some("expected-secret".to_string()));
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let (status, _resp) = http_request(
        addr,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Authorization", "Bearer wrong"),
        ],
        body,
    );
    assert_eq!(status, 401);
}
