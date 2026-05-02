// Integration smoke test against a running `crabcc serve`.
//
// Skips when nothing is listening on 127.0.0.1:7878 — keeps `cargo test`
// green for laptops that don't have the server up. CI doesn't run
// `crabcc-desktop` tests today (the crate is outside the workspace and
// has no smoke job yet), so this is a developer-loop tool.
//
// To run: in one shell `cargo run --release -p crabcc-cli -- serve`,
// then in another `cargo test -p crabcc-desktop --test api_smoke`.

use std::net::TcpStream;
use std::time::Duration;

use crabcc_desktop::api::Client;

const HOST_PORT: &str = "127.0.0.1:7878";

fn server_listening() -> bool {
    TcpStream::connect_timeout(
        &HOST_PORT.parse().expect("static addr parses"),
        Duration::from_millis(150),
    )
    .is_ok()
}

macro_rules! require_server {
    () => {
        if !server_listening() {
            eprintln!("api_smoke: skipping — no server on {HOST_PORT}");
            return;
        }
    };
}

#[test]
fn health_returns_ok() {
    require_server!();
    let resp = Client::new().health().expect("health call");
    assert_eq!(resp.status, "ok");
}

#[test]
fn bootstrap_has_repo_root_version() {
    require_server!();
    let bs = Client::new().bootstrap().expect("bootstrap call");
    assert!(!bs.repo.is_empty(), "repo populated");
    assert!(!bs.root.is_empty(), "root populated");
    assert!(!bs.version.is_empty(), "version populated");
}

#[test]
fn agents_endpoint_decodes() {
    require_server!();
    // Just check the wire shape decodes — agent count varies wildly
    // across local sessions so don't assert a length.
    let _ = Client::new().agents().expect("agents call");
}

#[test]
fn services_decodes() {
    require_server!();
    let report = Client::new().services().expect("services call");
    assert!(report.elapsed_ms < 60_000, "probe finished in <60s");
}
