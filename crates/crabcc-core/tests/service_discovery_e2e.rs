//! End-to-end integration suite for `service_discovery::discover_all` against
//! real containers (issue #161).
//!
//! Gated behind the `testcontainers` feature so the default `cargo test` path
//! stays Docker-free. Run with:
//!
//! ```sh
//! cargo test --features testcontainers -p crabcc-core --test service_discovery_e2e
//! ```
//!
//! Uses `serial_test::serial` because every case mutates `REDIS_URL` /
//! `CRABCC_COMPOSE` and `service_discovery` reads them at call time —
//! parallel cases would race the env mailbox.
//!
//! Phase 1 (this file): Redis only — 5 cases. Phase 2 (full Ollama +
//! LiteLLM stack) and Phase 3 (slow listener / dns-refused) tracked in #161.
#![cfg(feature = "testcontainers")]

use crabcc_core::service_discovery::discover_all;
use serial_test::serial;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

/// Pull the redis row out of a `DiscoveryReport`. Panics if the row is
/// missing — that's a contract regression we want to fail loudly on.
fn redis_row(
    report: &crabcc_core::service_discovery::DiscoveryReport,
) -> &crabcc_core::service_discovery::ServiceStatus {
    report
        .services
        .iter()
        .find(|s| s.service.name == "redis")
        .expect("`redis` row must exist in DiscoveryReport — known_services() contract")
}

/// Wait until `127.0.0.1:port` accepts a TCP connection or the deadline
/// expires. testcontainers' Redis module returns from `start().await` once
/// the container's stdout matches a ready pattern, but on macOS Docker
/// Desktop the host-side port-forwarding socket can take an extra
/// ~50-200 ms to become connectable. Without this wait, the first
/// `discover_all()` call sometimes hits ECONNREFUSED. Synthetic equivalent
/// of `wait-for-port` in the integration tests' bash precedent.
async fn wait_for_port(port: u16) {
    use std::net::TcpStream;
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let target = format!("127.0.0.1:{port}");
        if let Ok(addr) = target.parse() {
            if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("port {port} never opened within 5s");
}

/// Set REDIS_URL for the scope of a test, then unset on drop.
struct EnvGuard {
    key: &'static str,
    prior: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prior = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prior }
    }
    fn unset(key: &'static str) -> Self {
        let prior = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, prior }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

#[tokio::test]
#[serial]
async fn redis_up_via_env_url_is_reachable() {
    let container = Redis::default()
        .start()
        .await
        .expect("start redis container");
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("get redis host port");
    wait_for_port(port).await;

    let _redis = EnvGuard::set("REDIS_URL", &format!("redis://127.0.0.1:{port}"));
    let _compose = EnvGuard::unset("CRABCC_COMPOSE");

    let report = discover_all();
    let row = redis_row(&report);

    assert!(
        row.reachable,
        "redis at host port {port} should be reachable: {row:?}"
    );
    assert_eq!(row.service.source, "REDIS_URL");
    assert_eq!(row.service.host, "127.0.0.1");
    assert_eq!(row.service.port, port);
    assert!(row.error.is_none(), "no error expected: {row:?}");
}

#[tokio::test]
#[serial]
async fn redis_unreachable_after_container_stop() {
    let container = Redis::default()
        .start()
        .await
        .expect("start redis container");
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("get redis host port");
    wait_for_port(port).await;

    let _redis = EnvGuard::set("REDIS_URL", &format!("redis://127.0.0.1:{port}"));
    let _compose = EnvGuard::unset("CRABCC_COMPOSE");

    // Smoke that we *can* reach it before tearing it down — otherwise the
    // teardown assertion is a tautology.
    assert!(redis_row(&discover_all()).reachable);

    container.stop().await.expect("stop redis container");
    drop(container); // ensure the host-port mapping is released.

    let row = redis_row(&discover_all()).clone();
    assert!(
        !row.reachable,
        "redis should be unreachable after container stop: {row:?}"
    );
    assert!(
        row.error.is_some(),
        "stopped container should surface an error: {row:?}"
    );
}

#[tokio::test]
#[serial]
async fn redis_source_attribution_default_when_env_unset() {
    // No container — we just want to assert the source field is "default"
    // when REDIS_URL isn't set. The reachable flag is don't-care here:
    // either nothing's on 6379 (false) or some other dev's redis is (true).
    let _redis = EnvGuard::unset("REDIS_URL");
    let _compose = EnvGuard::unset("CRABCC_COMPOSE");

    let report = discover_all();
    let row = redis_row(&report);

    assert_eq!(row.service.source, "default");
    assert_eq!(row.service.url, "redis://127.0.0.1:6379");
}

#[tokio::test]
#[serial]
async fn redis_dns_refused_when_url_points_at_invalid_host() {
    let _redis = EnvGuard::set("REDIS_URL", "redis://does-not-exist.invalid:6379");
    let _compose = EnvGuard::unset("CRABCC_COMPOSE");

    let report = discover_all();
    let row = redis_row(&report);

    assert!(
        !row.reachable,
        "invalid hostname should not resolve: {row:?}"
    );
    assert!(
        row.error.is_some(),
        "DNS failure should surface an error: {row:?}"
    );
}

#[tokio::test]
#[serial]
async fn redis_tcp_only_listener_still_probes_reachable() {
    use std::net::TcpListener;

    // Bind an ephemeral listener that accepts the connect but never speaks
    // RESP — service_discovery does a pure TCP probe (no protocol-level
    // handshake), so this must still surface as `reachable = true`. Guards
    // against accidentally tightening probe_service to require a banner.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().unwrap().port();

    let _redis = EnvGuard::set("REDIS_URL", &format!("redis://127.0.0.1:{port}"));
    let _compose = EnvGuard::unset("CRABCC_COMPOSE");

    let report = discover_all();
    let row = redis_row(&report);

    assert!(
        row.reachable,
        "TCP-listening port should be reachable even without a Redis protocol response: {row:?}"
    );
}
