//! Service discovery — resolve URLs for redis / litellm / etc. with the
//! same rules as `crabcc_core::service_discovery` (the canonical
//! registry at `crates/crabcc-core/src/service_discovery.rs`).
//!
//! Resolution order (highest precedence wins):
//!   1. The service's explicit env var (`REDIS_URL`, `LITELLM_BASE_URL`, …).
//!   2. `CRABCC_COMPOSE=1` → compose-network service name + default port.
//!   3. `127.0.0.1` + default port.
//!
//! Why a local replica vs path-dep'ing crabcc-core: this crate is a
//! standalone (own `[workspace]`) for build-isolation reasons, and
//! crabcc-core drags in rusqlite + tantivy + tree-sitter — too heavy
//! for what amounts to ~30 lines of host-substitution logic. The
//! canonical contract lives in crabcc-core; bumps there should mirror
//! here. A future `crabcc-discovery` micro-crate would let both
//! consume one source of truth — tracked, not blocked on this PR.

use tracing::debug;

#[derive(Debug, Clone)]
pub struct Resolved {
    pub url: String,
    pub source: Source,
}

#[derive(Debug, Clone, Copy)]
pub enum Source {
    /// URL came from the explicit env var — always wins.
    EnvVar,
    /// URL came from the `CRABCC_COMPOSE=1` default (compose-network names).
    ComposeDefault,
    /// URL came from the localhost default.
    LocalDefault,
}

pub fn resolve_redis() -> Resolved {
    resolve("REDIS_URL", "redis", "redis", 6379, "redis://", "")
}

pub fn resolve_litellm() -> Resolved {
    resolve("LITELLM_BASE_URL", "litellm", "litellm", 4000, "http://", "")
}

fn resolve(
    env_var: &str,
    _service_name: &str,
    compose_name: &str,
    default_port: u16,
    scheme: &str,
    path: &str,
) -> Resolved {
    if let Ok(v) = std::env::var(env_var) {
        if !v.is_empty() {
            return Resolved {
                url: v,
                source: Source::EnvVar,
            };
        }
    }
    let compose_mode = matches!(std::env::var("CRABCC_COMPOSE").as_deref(), Ok("1"));
    let (host, source) = if compose_mode {
        (compose_name, Source::ComposeDefault)
    } else {
        ("127.0.0.1", Source::LocalDefault)
    };
    let url = format!("{scheme}{host}:{default_port}{path}");
    debug!(target: "crabcc_agents::discovery", env_var, url = %url, ?source, "resolved");
    Resolved { url, source }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Source::EnvVar => "env",
            Source::ComposeDefault => "compose-default",
            Source::LocalDefault => "local-default",
        })
    }
}
