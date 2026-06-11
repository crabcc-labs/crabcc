//! Smoke test for `crabcc serve` (issue #64).
//!
//! Boots the server on an ephemeral port (port=0 → kernel-picked) against
//! a temp repo containing one tiny Rust file, then round-trips three
//! requests over a raw `TcpStream` (no reqwest dep needed):
//!   1. GET /api/health   → 200 with `{ "status": "ok" }`
//!   2. GET /             → 200 HTML containing a known marker string
//!   3. GET /api/graph    → 200 JSON with the root node + at least one edge
//!
//! Uses `bind_listener` + `serve_with_listener` so the test learns the
//! resolved port up front; spawns the request loop on a background thread.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use crabcc_core::index::full_index;
use crabcc_core::store::Store;

/// Returns (root, port). Spawns the server thread; the test process exit
/// will tear it down (tiny_http listens on a TcpListener owned by the
/// thread; dropping the process drops the FD).
fn boot_test_server() -> (tempfile::TempDir, u16) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();

    // Realistic fixture: one Rust file with two functions, one calling the
    // other. After `full_index` the call-graph has a single edge, which is
    // enough to exercise the `/api/graph` snapshot path end-to-end.
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::write(
        src.join("lib.rs"),
        "pub fn outer() { inner(); }\npub fn inner() { let _ = 1; }\n",
    )
    .expect("write src");

    let db_path = root.join(".crabcc").join("index.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).expect("mkdir .crabcc");
    let store = Store::open(&db_path).expect("open store");
    full_index(&root, &store).expect("full_index");
    drop(store); // close so the server's lazy open can re-acquire the file

    let listener =
        crabcc_viz::bind_listener(IpAddr::V4(Ipv4Addr::LOCALHOST), 0).expect("bind ephemeral");
    let port = listener.local_addr().expect("local_addr").port();
    let root_for_thread: PathBuf = root.clone();
    std::thread::spawn(move || {
        // Errors from the server thread surface via the test failing on the
        // round-trip — there's nothing useful to do with them here.
        let _ = crabcc_viz::serve_with_listener(listener, &root_for_thread);
    });
    // tiny_http binds synchronously inside `Server::from_listener`, but
    // give the worker thread a beat to enter `incoming_requests` before
    // we slam it with a connect.
    std::thread::sleep(Duration::from_millis(50));
    (dir, port)
}

fn http_get(port: u16, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set_read_timeout");
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).expect("write request");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read response");
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status code");
    (status, body.to_string())
}

#[test]
fn health_endpoint_returns_ok() {
    let (_dir, port) = boot_test_server();
    let (status, body) = http_get(port, "/api/health");
    assert_eq!(status, 200, "health endpoint should return 200");
    assert!(
        body.contains("\"status\""),
        "health body should contain status field: {body}"
    );
    assert!(
        body.contains("\"ok\""),
        "health body should report ok: {body}"
    );
}

#[test]
fn root_redirects_to_app_eye() {
    // The live dashboard moved to the standalone app-eye deployment.
    // / (and its aliases) now 302-redirect to dashb.crabcc.app so
    // human visitors land on the real dashboard. The interactive
    // call-graph viewer still lives at /graph.
    let (_dir, port) = boot_test_server();
    let (status, _body) = http_get(port, "/");
    assert_eq!(status, 302, "root should redirect to app-eye (302)");

    // The interactive graph viewer is at /graph and still serves HTML.
    let (graph_status, graph_body) = http_get(port, "/graph");
    assert_eq!(graph_status, 200, "graph endpoint should return 200");
    assert!(
        graph_body.contains("<canvas"),
        "/graph body should contain the graph canvas: {graph_body:.200}"
    );
}

#[test]
fn graph_endpoint_returns_root_node_and_edges() {
    let (_dir, port) = boot_test_server();
    // Walk callers of `inner` — there's exactly one (`outer`).
    let (status, body) = http_get(port, "/api/graph?root=inner&dir=callers&depth=2");
    assert_eq!(
        status, 200,
        "graph endpoint should return 200, got body: {body}"
    );
    let v: serde_json::Value =
        serde_json::from_str(&body).expect("graph endpoint must return valid JSON");
    assert_eq!(v["root"], "inner");
    assert_eq!(v["dir"], "callers");
    let nodes = v["nodes"].as_array().expect("nodes array");
    assert!(
        nodes.iter().any(|n| n["id"] == "inner" && n["depth"] == 0),
        "snapshot must contain root node at depth 0: {body}"
    );
    assert!(
        nodes.iter().any(|n| n["id"] == "outer"),
        "snapshot must contain caller `outer`: {body}"
    );
    let edges = v["edges"].as_array().expect("edges array");
    assert!(
        edges
            .iter()
            .any(|e| e["src"] == "outer" && e["dst"] == "inner"),
        "snapshot must contain outer→inner edge: {body}"
    );
}

#[test]
fn graph_endpoint_rejects_missing_root() {
    let (_dir, port) = boot_test_server();
    let (status, body) = http_get(port, "/api/graph?dir=callers&depth=2");
    assert_eq!(
        status, 400,
        "missing root should be a 400, got body: {body}"
    );
}

#[test]
fn graph_endpoint_rejects_bad_dir() {
    let (_dir, port) = boot_test_server();
    let (status, body) = http_get(port, "/api/graph?root=inner&dir=sideways&depth=2");
    assert_eq!(status, 400, "bad dir should be a 400, got body: {body}");
}

#[test]
fn unknown_route_is_404() {
    let (_dir, port) = boot_test_server();
    let (status, _body) = http_get(port, "/does-not-exist");
    assert_eq!(status, 404);
}

#[test]
fn live_route_redirects_to_app_eye() {
    // The live dashboard moved out of crabcc-viz into the standalone
    // app-eye deployment. /live (and /index.html) must 302-redirect to
    // dashb.crabcc.app so existing bookmarks still reach the dashboard.
    let (_dir, port) = boot_test_server();
    let (status, _body) = http_get(port, "/live");
    assert_eq!(status, 302, "/live should redirect to app-eye (302)");

    // / is an alias; it should also redirect.
    let (idx_status, _idx_body) = http_get(port, "/");
    assert_eq!(idx_status, 302, "/ must also redirect to app-eye (302)");
}

#[test]
fn eye_preview_is_invisible_without_token() {
    // The /eye agent-monitor preview is private. With CRABCC_EYE_TOKEN unset
    // (the default) it MUST 404 — the bundled dashboard never leaks to anyone
    // who doesn't hold the token. Guards the "preview only for the operator"
    // contract against an accidental always-on regression.
    assert!(std::env::var("CRABCC_EYE_TOKEN").is_err(), "test env must not set CRABCC_EYE_TOKEN");
    let (_dir, port) = boot_test_server();
    let (status, _body) = http_get(port, "/eye");
    assert_eq!(status, 404, "/eye must be invisible (404) when CRABCC_EYE_TOKEN is unset");
}

#[test]
fn bootstrap_endpoint_returns_repo_summary() {
    let (_dir, port) = boot_test_server();
    let (status, body) = http_get(port, "/api/bootstrap");
    assert_eq!(status, 200, "bootstrap must succeed: {body}");
    let v: serde_json::Value =
        serde_json::from_str(&body).expect("bootstrap must return valid JSON");
    // Shape contract — the live frontend depends on these keys + types.
    assert!(v.get("repo").is_some(), "missing repo: {body}");
    assert!(v.get("root").is_some(), "missing root: {body}");
    assert!(v.get("version").is_some(), "missing version: {body}");
    let index = v.get("index").expect("missing index block");
    assert!(index["present"].as_bool().expect("present must be a bool"));
    // The fixture has 2 functions in one Rust file → at least 1 file
    // and at least 2 symbols. Use ≥ assertions so the test stays
    // robust against extractor evolution that might add synthetic
    // module-level symbols.
    assert!(index["files"].as_u64().unwrap_or_default() >= 1);
    assert!(index["symbols"].as_u64().unwrap_or_default() >= 2);
    let graph = v.get("graph").expect("missing graph block");
    assert!(graph.get("present").is_some());
    let memory = v.get("memory").expect("missing memory block");
    assert!(memory.get("present").is_some());
}

#[test]
fn memory_recent_endpoint_returns_empty_when_db_absent() {
    let (_dir, port) = boot_test_server();
    let (status, body) = http_get(port, "/api/memory/recent?since=0&limit=5");
    assert_eq!(status, 200);
    let v: serde_json::Value =
        serde_json::from_str(&body).expect("memory.recent must return valid JSON");
    assert!(v.get("present").is_some());
    assert!(
        v["drawers"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or_default(),
        "drawers must be empty when no memory.db: {body}"
    );
}

#[test]
fn activity_endpoint_returns_snapshot_shape() {
    let (_dir, port) = boot_test_server();
    let (status, body) = http_get(port, "/api/activity?since=0&limit=10");
    assert_eq!(status, 200, "activity must succeed: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).expect("activity must be valid JSON");
    // Shape contract: `repo`, `cursor`, `events` are always present, and
    // `events` is always an array (possibly empty when there's been no
    // recorded activity for this repo). The frontend depends on this.
    assert!(v.get("repo").is_some(), "missing repo: {body}");
    assert!(v.get("cursor").is_some(), "missing cursor: {body}");
    assert!(
        v.get("events").and_then(|e| e.as_array()).is_some(),
        "events must be an array: {body}"
    );
}
