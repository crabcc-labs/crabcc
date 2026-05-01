//! LiteLLM preflight — at worker boot, probe `LITELLM_BASE_URL` so a
//! mis-configured stack fails loudly here instead of mid-prompt inside
//! a per-job container.
//!
//! We deliberately don't duplicate the LiteLLM container spec — the
//! repo already maintains it at `install/ollama-stack/docker-compose.yml`
//! (issue #105). The agents service preflights it; bringing it up is
//! `task ollama-stack-up` (or `task -d apps/crabcc-agents litellm:check-build-run`,
//! which is just a thin wrapper).
//!
//! Probe is a TCP connect with a short timeout — cheap, decoupled from
//! the LiteLLM HTTP surface, and accurate enough to catch the common
//! "user forgot to start the stack" failure mode. A future health-
//! aware probe (`GET /health`) can layer on top without changing the
//! preflight contract.

use anyhow::{anyhow, Context, Result};
use std::net::ToSocketAddrs;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{info, warn};

use crate::config::Config;

/// Probe outcome. Reachable does not imply healthy — only that
/// something accepts TCP on the configured host:port.
pub enum Status {
    Reachable,
    Unreachable(String),
    Disabled,
}

pub async fn preflight(cfg: &Config) -> Status {
    let Some(url) = &cfg.litellm_base_url else {
        return Status::Disabled;
    };

    match probe(url, cfg.litellm_probe_timeout).await {
        Ok(()) => {
            info!(target: "crabcc_agents::litellm", url = %url, "LiteLLM reachable");
            Status::Reachable
        }
        Err(e) => {
            warn!(target: "crabcc_agents::litellm", url = %url, %e, "LiteLLM preflight failed");
            Status::Unreachable(e.to_string())
        }
    }
}

/// Convenience: ensure LiteLLM is reachable, hard-fail if `required`.
pub async fn ensure(cfg: &Config) -> Result<()> {
    let required = cfg.litellm_required;
    match preflight(cfg).await {
        Status::Reachable | Status::Disabled => Ok(()),
        Status::Unreachable(why) if required => Err(anyhow!(
            "LiteLLM required but unreachable: {why}. \
             Bring it up with `task ollama-stack-up`, \
             or unset LITELLM_REQUIRED to continue without."
        )),
        Status::Unreachable(_) => Ok(()),
    }
}

async fn probe(url: &str, dur: Duration) -> Result<()> {
    // Strip scheme + path, keep host:port.
    let host_port = url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .ok_or_else(|| anyhow!("LITELLM_BASE_URL has no host"))?;

    // Resolve + connect with a hard timeout — never block the worker
    // boot path on a stuck DNS lookup or a half-open TCP.
    let addr = host_port
        .to_socket_addrs()
        .with_context(|| format!("resolve {host_port}"))?
        .next()
        .ok_or_else(|| anyhow!("no addrs for {host_port}"))?;

    timeout(dur, TcpStream::connect(addr))
        .await
        .with_context(|| format!("connect to {host_port} timed out after {:?}", dur))?
        .with_context(|| format!("connect to {host_port}"))?;

    Ok(())
}
