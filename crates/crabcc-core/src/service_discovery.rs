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
use std::path::{Path, PathBuf};
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
    /// crabcc MCP server reachable over HTTP/SSE — `crabcc --mcp-http :PORT`.
    /// Phase 0 of #204; transport implementation lands in a follow-up PR.
    /// Distinct kind (vs HttpJsonApi) so consumers (bot, viz dashboard,
    /// menubar Services panel) can render MCP-specific affordances.
    Mcp,
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
            "crabcc-mcp",
            ServiceKind::Mcp,
            "CRABCC_MCP_URL",
            &format!("http://{}:8091/mcp", h!("crabcc-mcp", "127.0.0.1")),
        ),
        resolve_service(
            "crabcc-hitl",
            ServiceKind::HttpJsonApi,
            "CRABCC_HITL_URL",
            &format!("http://{}:9100", h!("crabcc-hitl", "127.0.0.1")),
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
        // crabcc MCP HTTP — distinct port so it doesn't collide with
        // crabcc-serve (8090) or telegram-bot-web (8092). #204.
        ServiceKind::Mcp => 8091,
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

    tracing::debug!(
        target: "crabcc_core::service_discovery",
        service = %svc.name,
        host = %svc.host,
        port = svc.port,
        "probe: start"
    );

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
        Ok(_) => {
            tracing::info!(
                target: "crabcc_core::service_discovery",
                service = %svc.name,
                reachable = true,
                latency_ms,
                "probe: ok"
            );
            ServiceStatus {
                service: svc.clone(),
                reachable: true,
                latency_ms,
                error: None,
                probed_at,
            }
        }
        Err(e) => {
            // Down state is normal during dev (the stack may not be up yet),
            // so we log at `info` level instead of `warn`. Operators can lift
            // this to `warn` via RUST_LOG=crabcc_core::service_discovery=warn.
            tracing::info!(
                target: "crabcc_core::service_discovery",
                service = %svc.name,
                reachable = false,
                latency_ms,
                error = %e,
                "probe: down"
            );
            ServiceStatus {
                service: svc.clone(),
                reachable: false,
                latency_ms,
                error: Some(e),
                probed_at,
            }
        }
    }
}

/// Filename of the on-disk sidecar written by `crabcc serve` (issue #143).
/// Lives at `<repo>/.crabcc/services.json`.
pub const SIDECAR_FILE: &str = "services.json";

/// Resolve the canonical sidecar path under `<repo>/.crabcc/`.
pub fn sidecar_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".crabcc").join(SIDECAR_FILE)
}

/// Persist a discovery report to `<repo>/.crabcc/services.json`.
/// Best-effort: returns `Err` only if both the parent dir and the write
/// failed. Used by `crabcc serve` startup so other host processes can
/// read what we resolved without re-running the probe themselves.
pub fn write_sidecar(repo_root: &Path, report: &DiscoveryReport) -> std::io::Result<PathBuf> {
    let path = sidecar_path(repo_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(report)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    std::fs::write(&path, body)?;
    Ok(path)
}

/// Read a previously-written sidecar back into a `DiscoveryReport`.
pub fn read_sidecar(repo_root: &Path) -> std::io::Result<DiscoveryReport> {
    let path = sidecar_path(repo_root);
    let body = std::fs::read_to_string(&path)?;
    serde_json::from_str(&body).map_err(|e| std::io::Error::other(format!("parse: {e}")))
}

/// Discover + probe every known service. Bounded by `PROBE_TIMEOUT` per
/// service (currently sequential — fine for ~7 services).
pub fn discover_all() -> DiscoveryReport {
    let start = Instant::now();
    let compose_mode = is_compose_mode();

    tracing::info!(
        target: "crabcc_core::service_discovery",
        compose_mode,
        "discover_all: start"
    );

    let services = known_services();
    let statuses: Vec<ServiceStatus> = services.iter().map(probe_service).collect();
    let elapsed_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let up = statuses.iter().filter(|s| s.reachable).count();

    tracing::info!(
        target: "crabcc_core::service_discovery",
        services = statuses.len(),
        up,
        down = statuses.len() - up,
        elapsed_ms,
        compose_mode,
        "discover_all: complete"
    );

    DiscoveryReport {
        services: statuses,
        compose_mode,
        elapsed_ms,
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
        assert_eq!(default_port_for(&ServiceKind::Mcp), 8091);
    }

    #[test]
    #[ignore = "TCP probe — flake-prone if firewall delays connect-refused; run locally with --ignored"]
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
    #[ignore = "binds ephemeral TCP — flake-prone on parallel test runs; run locally with --ignored"]
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
        for required in [
            "redis",
            "litellm",
            "ollama",
            "rotel-grpc",
            "crabcc-serve",
            "crabcc-mcp",
        ] {
            assert!(
                names.contains(&required),
                "expected '{}' in known_services, got {:?}",
                required,
                names
            );
        }
    }

    #[test]
    fn crabcc_mcp_default_url_uses_loopback_outside_compose() {
        // Sanity: outside compose mode the MCP entry resolves to
        // 127.0.0.1:8091 with the /mcp path. Bot reaches this via
        // `host.docker.internal` from inside its container.
        // Note: this asserts the default — env override (CRABCC_MCP_URL)
        // is intentionally not tested here to avoid env-var pollution
        // across parallel test runs.
        let services = known_services();
        let mcp = services
            .iter()
            .find(|s| s.name == "crabcc-mcp")
            .expect("crabcc-mcp entry");
        assert_eq!(mcp.kind, ServiceKind::Mcp);
        assert_eq!(mcp.port, 8091);
        // Host depends on CRABCC_COMPOSE — assert the loopback or
        // compose-network value, accepting either since CI may set
        // CRABCC_COMPOSE for some test profiles.
        assert!(
            mcp.host == "127.0.0.1" || mcp.host == "crabcc-mcp",
            "unexpected host: {}",
            mcp.host
        );
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
