//! Service discovery — enumerate the known crabcc services + their resolved
//! URLs and probe each for reachability.
//!
//! Used by:
//!   - `crabcc debug-service-discovery` (CLI)
//!   - `/api/services` on `crabcc serve` (viz dashboard)
//!
//! The probe is a TCP connect with a short (800 ms) timeout — enough to
//! tell "service is reachable" from "port is closed / DNS missing" without
//! blocking the menubar / dashboard for seconds. We intentionally do NOT
//! attempt a protocol-level probe (e.g. `redis-cli ping`, `HEAD /`) here —
//! that's the job of `crabcc doctor stack` / `crabcc doctor jobs`.
//!
//! When `CRABCC_COMPOSE=1` is set in the environment (the docker-compose
//! file exports it), the default URLs use compose-network service names
//! (`redis`, `litellm`, `rotel`, …) instead of `127.0.0.1`. Explicit env
//! vars (`REDIS_URL`, `OLLAMA_HOST`, …) always win over the compose default.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const PROBE_TIMEOUT: Duration = Duration::from_millis(800);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    Redis,
    HttpJsonApi,
    OtlpGrpc,
    OtlpHttp,
    Ollama,
    Generic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    pub name: String,
    pub kind: ServiceKind,
    pub url: String,
    /// Where the URL came from: `"default"` or the env var name (e.g. `"REDIS_URL"`).
    pub source: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    #[serde(flatten)]
    pub service: Service,
    pub reachable: bool,
    /// TCP-connect latency in ms (set even on failure — represents the
    /// time spent before the timeout / refusal).
    pub latency_ms: u64,
    pub error: Option<String>,
    pub probed_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiscoveryReport {
    pub services: Vec<ServiceStatus>,
    /// True when `CRABCC_COMPOSE=1` is in the environment — defaults
    /// switch to compose-network service names instead of localhost.
    pub compose_mode: bool,
    pub elapsed_ms: u64,
}

/// Build the canonical list of services with their resolved URLs.
/// Reads env vars at call time — safe to call repeatedly.
pub fn known_services() -> Vec<Service> {
    let compose = is_compose_mode();
    // Pick host: compose-network name vs local loopback. Plain `&str` ternary
    // can't unify the lifetimes of two distinct literals through a closure
    // — using `if … else …` inline at each call site keeps it borrow-check
    // simple.
    macro_rules! h {
        ($compose:expr, $local:expr) => {
            if compose {
                $compose
            } else {
                $local
            }
        };
    }

    vec![
        resolve_service(
            "redis",
            ServiceKind::Redis,
            "REDIS_URL",
            &format!("redis://{}:6379", h!("redis", "127.0.0.1")),
        ),
        resolve_service(
            "litellm",
            ServiceKind::HttpJsonApi,
            "LITELLM_BASE_URL",
            &format!("http://{}:4000", h!("litellm", "127.0.0.1")),
        ),
        resolve_service(
            "ollama",
            ServiceKind::Ollama,
            "OLLAMA_HOST",
            &format!("http://{}:11434", h!("ollama", "127.0.0.1")),
        ),
        resolve_service(
            "rotel-grpc",
            ServiceKind::OtlpGrpc,
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            &format!("http://{}:4317", h!("rotel", "127.0.0.1")),
        ),
        resolve_service(
            "rotel-http",
            ServiceKind::OtlpHttp,
            "OTEL_EXPORTER_OTLP_HTTP_ENDPOINT",
            &format!("http://{}:4318", h!("rotel", "127.0.0.1")),
        ),
        resolve_service(
            "crabcc-serve",
            ServiceKind::HttpJsonApi,
            "CRABCC_SERVE_URL",
            &format!("http://{}:8090", h!("crabcc-serve", "127.0.0.1")),
        ),
        resolve_service(
            "telegram-bot-web",
            ServiceKind::HttpJsonApi,
            "TELEGRAM_BOT_WEB_URL",
            &format!("http://{}:8092", h!("crabcc-telegram", "127.0.0.1")),
        ),
    ]
}

fn is_compose_mode() -> bool {
    matches!(std::env::var("CRABCC_COMPOSE").as_deref(), Ok("1"))
}

fn resolve_service(name: &str, kind: ServiceKind, env_var: &str, default_url: &str) -> Service {
    let (url, source) = match std::env::var(env_var) {
        Ok(v) if !v.is_empty() => (v, env_var.to_string()),
        _ => (default_url.to_string(), "default".to_string()),
    };
    let (host, port) = parse_host_port(&url, default_port_for(&kind));
    Service {
        name: name.to_string(),
        kind,
        url,
        source,
        host,
        port,
    }
}

fn default_port_for(kind: &ServiceKind) -> u16 {
    match kind {
        ServiceKind::Redis => 6379,
        ServiceKind::Ollama => 11434,
        ServiceKind::OtlpGrpc => 4317,
        ServiceKind::OtlpHttp => 4318,
        ServiceKind::HttpJsonApi => 80,
        ServiceKind::Generic => 80,
    }
}

/// Parse host + port from a URL of shape `scheme://host[:port][/path]`.
/// Falls back to `default_port` when no port is in the URL.
fn parse_host_port(url: &str, default_port: u16) -> (String, u16) {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
    if let Some((h, p)) = host_port.rsplit_once(':') {
        if let Ok(port) = p.parse::<u16>() {
            return (h.to_string(), port);
        }
    }
    (host_port.to_string(), default_port)
}

/// Probe a service: TCP-connect to host:port with `PROBE_TIMEOUT`.
pub fn probe_service(svc: &Service) -> ServiceStatus {
    let start = Instant::now();
    let probed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let addr_str = format!("{}:{}", svc.host, svc.port);
    let result = addr_str
        .to_socket_addrs()
        .map_err(|e| format!("dns: {e}"))
        .and_then(|mut iter| iter.next().ok_or_else(|| "no addr".to_string()))
        .and_then(|addr| {
            TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).map_err(|e| format!("tcp: {e}"))
        });

    let latency_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    match result {
        Ok(_) => ServiceStatus {
            service: svc.clone(),
            reachable: true,
            latency_ms,
            error: None,
            probed_at,
        },
        Err(e) => ServiceStatus {
            service: svc.clone(),
            reachable: false,
            latency_ms,
            error: Some(e),
            probed_at,
        },
    }
}

/// Discover + probe every known service. Bounded by `PROBE_TIMEOUT` per
/// service (currently sequential — fine for ~7 services).
pub fn discover_all() -> DiscoveryReport {
    let start = Instant::now();
    let services = known_services();
    let statuses: Vec<ServiceStatus> = services.iter().map(probe_service).collect();
    DiscoveryReport {
        services: statuses,
        compose_mode: is_compose_mode(),
        elapsed_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn parse_host_port_with_explicit_port() {
        let (h, p) = parse_host_port("redis://127.0.0.1:6379", 0);
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, 6379);
    }

    #[test]
    fn parse_host_port_uses_default_when_missing() {
        let (h, p) = parse_host_port("http://example.com/api", 80);
        assert_eq!(h, "example.com");
        assert_eq!(p, 80);
    }

    #[test]
    fn parse_host_port_strips_path() {
        let (h, p) = parse_host_port("http://localhost:8080/foo/bar", 0);
        assert_eq!(h, "localhost");
        assert_eq!(p, 8080);
    }

    #[test]
    fn parse_host_port_no_scheme() {
        let (h, p) = parse_host_port("redis:6379", 0);
        assert_eq!(h, "redis");
        assert_eq!(p, 6379);
    }

    #[test]
    fn default_port_per_kind() {
        assert_eq!(default_port_for(&ServiceKind::Redis), 6379);
        assert_eq!(default_port_for(&ServiceKind::Ollama), 11434);
        assert_eq!(default_port_for(&ServiceKind::OtlpGrpc), 4317);
        assert_eq!(default_port_for(&ServiceKind::OtlpHttp), 4318);
    }

    #[test]
    fn probe_unreachable_port_returns_error() {
        // Port 1 is reserved + reliably refused on every OS.
        let svc = Service {
            name: "fake".into(),
            kind: ServiceKind::Generic,
            url: "http://127.0.0.1:1".into(),
            source: "default".into(),
            host: "127.0.0.1".into(),
            port: 1,
        };
        let s = probe_service(&svc);
        assert!(!s.reachable, "port 1 should never be reachable");
        assert!(s.error.is_some());
    }

    #[test]
    fn probe_reachable_port_returns_ok() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let port = listener.local_addr().unwrap().port();
        let svc = Service {
            name: "test".into(),
            kind: ServiceKind::Generic,
            url: format!("http://127.0.0.1:{}", port),
            source: "default".into(),
            host: "127.0.0.1".into(),
            port,
        };
        let s = probe_service(&svc);
        assert!(s.reachable, "ephemeral listener should be reachable");
        assert!(s.error.is_none());
    }

    #[test]
    fn known_services_contains_core_set() {
        let services = known_services();
        let names: Vec<&str> = services.iter().map(|s| s.name.as_str()).collect();
        for required in ["redis", "litellm", "ollama", "rotel-grpc", "crabcc-serve"] {
            assert!(
                names.contains(&required),
                "expected '{}' in known_services, got {:?}",
                required,
                names
            );
        }
    }

    #[test]
    fn discover_all_returns_a_report() {
        let r = discover_all();
        assert!(!r.services.is_empty());
        assert!(r.elapsed_ms < 30_000, "probes should be bounded");
    }

    #[test]
    fn service_status_serializes_with_flat_service_fields() {
        let s = ServiceStatus {
            service: Service {
                name: "x".into(),
                kind: ServiceKind::Redis,
                url: "redis://127.0.0.1:6379".into(),
                source: "default".into(),
                host: "127.0.0.1".into(),
                port: 6379,
            },
            reachable: true,
            latency_ms: 12,
            error: None,
            probed_at: 1730000000,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["name"], "x");
        assert_eq!(v["kind"], "redis");
        assert_eq!(v["reachable"], true);
        assert_eq!(v["latency_ms"], 12);
    }
}
