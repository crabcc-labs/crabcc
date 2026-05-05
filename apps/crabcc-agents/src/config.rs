use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub redis_url: String,
    pub queue_name: String,
    pub concurrency: usize,
    pub poll_ms: u64,
    pub health_addr: SocketAddr,

    pub agent_image: String,
    pub agent_memory_bytes: i64,
    pub agent_cpu_quota: i64,
    pub agent_cpu_period: i64,
    pub agent_shm_bytes: i64,
    pub agent_pids_limit: i64,
    pub agent_tmpfs_workspace_bytes: i64,
    pub agent_tmpfs_tmp_bytes: i64,
    pub agent_timeout_secs: u64,

    /// Default model passed to `claude code --model` if the job
    /// payload doesn't override.
    pub default_model: String,
    /// Default reasoning effort: `high` | `medium` | `low`.
    pub default_effort: String,

    /// Host-side path to the Claude Code SSO credentials file. Bind-
    /// mounted read-only into each agent container so the in-container
    /// `claude` CLI authenticates as the same logged-in user without
    /// shipping a token via env. Resolved via `HOST_CLAUDE_CREDENTIALS`
    /// or defaulted to `$HOME/.claude/.credentials.json`. Set empty to
    /// disable.
    pub host_claude_credentials: Option<PathBuf>,

    /// Optional pre-extracted OAuth token. Used as a fallback when the
    /// credentials file is unavailable (e.g. macOS Keychain-only
    /// hosts). Passed as `CLAUDE_CODE_OAUTH_TOKEN` into the agent.
    pub claude_oauth_token: Option<String>,

    /// HTTP MCP URL of an axint-mcp-http running on the *host*. When
    /// set, agent containers connect over HTTP to share the host's
    /// warm axint state (project memory pack, registry cache, fix-
    /// packet history) instead of cold-starting a fresh `axint mcp`
    /// stdio process per job. Example: `http://host.docker.internal:7785/mcp`.
    pub host_axint_mcp_url: Option<String>,

    /// Host-side path to the user's `.crabcc/` directory — symbol
    /// index, memory.db, agent run logs, scenarios. Bind-mounted
    /// **read-only** at `/home/nonroot/.crabcc/` inside agent
    /// containers so the in-container `crabcc` CLI can read the
    /// symbol index and memory drawers. Resolved via
    /// `HOST_CRABCC_DIR` or defaulted to `$HOME/.crabcc`. Set the
    /// env var to empty to disable. Read-only is intentional: a
    /// containerised agent shouldn't mutate the host's symbol index
    /// or memory drawers without explicit invocation through the
    /// MCP socket (where the host can audit + gate).
    pub host_crabcc_dir: Option<PathBuf>,

    /// Docker network mode for agent containers when the host axint
    /// URL is configured. Defaults to `bridge` so the container can
    /// resolve `host.docker.internal`. Override to a dedicated
    /// network (e.g. `crabcc-agents-egress`) to narrow the egress
    /// blast radius — see README.
    pub agent_network: String,

    pub stream_maxlen: usize,
    pub stream_key_prefix: String,

    /// Smoke / pipeline-test mode. When set the worker swaps the
    /// agent image to `alpine:3.20` and the command to a shell echo
    /// using the prompt — exercises the full BullMQ → Docker → Redis
    /// Streams path without spending Anthropic tokens or needing an
    /// agent-runner image. Strictly off in production.
    pub smoke: bool,

    /// Pre-warm the agent image at worker boot — inspect locally;
    /// pull from the registry only if missing. Surfaces a missing
    /// image at startup instead of mid-job, and primes the daemon's
    /// layer cache so the first job doesn't pay the pull cost.
    pub prewarm: bool,

    /// Tokio worker thread cap. Lower than the host core count for a
    /// dispatcher-style workload (the worker mostly waits on Redis +
    /// Docker IO; there's no CPU-bound work). Default = min(host_cores, 4).
    pub tokio_worker_threads: usize,

    /// LiteLLM proxy URL (`LITELLM_BASE_URL`). When set, the worker
    /// TCP-probes it at boot. Set empty / `-` / `none` to disable the
    /// probe entirely (e.g. when running against direct Anthropic
    /// without the LiteLLM stack).
    pub litellm_base_url: Option<String>,

    /// Hard-fail boot when LiteLLM is configured but unreachable.
    /// `LITELLM_REQUIRED` (default off — preflight logs a warning,
    /// worker keeps running). Flip on in production where missing
    /// LiteLLM is a config error not a transient state.
    pub litellm_required: bool,

    /// Preflight TCP-connect timeout. Short by default — the probe
    /// shouldn't slow boot when LiteLLM is up; if it's down, bail
    /// fast instead of blocking on DNS/TCP retries.
    pub litellm_probe_timeout: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        // Service URLs go through the discovery module so resolution
        // matches crabcc_core::service_discovery (compose-mode aware,
        // env var beats defaults). See `discovery.rs` for the contract.
        let redis = crate::discovery::resolve_redis();
        let litellm = crate::discovery::resolve_litellm();
        eprintln!(
            "crabcc-agents: discovery — redis@{} ({}), litellm@{} ({})",
            redis.url, redis.source, litellm.url, litellm.source
        );
        Ok(Self {
            redis_url: redis.url,
            queue_name: env_or("AGENTS_QUEUE", "crabcc:agents"),
            concurrency: env_parse("AGENTS_CONCURRENCY", 4)?,
            // Lower poll interval = faster pickup. 50 ms is the sweet
            // spot under empty-queue conditions: a single BLPOP wake-
            // up cost of ~0.05 ms × 20 Hz × 4 workers ≈ 4 ms/s of CPU,
            // and median pickup latency of ~25 ms beats user
            // perception threshold. Override via AGENTS_POLL_MS for
            // bursty workloads.
            poll_ms: env_parse("AGENTS_POLL_MS", 50)?,
            health_addr: env_or("AGENTS_HEALTH_ADDR", "0.0.0.0:9090")
                .parse()
                .context("AGENTS_HEALTH_ADDR")?,

            agent_image: env_or(
                "AGENT_IMAGE",
                "ghcr.io/peterlodri-sec/crabcc-agent-runner:latest",
            ),
            agent_memory_bytes: env_parse("AGENT_MEMORY_BYTES", 2 * 1024 * 1024 * 1024)?,
            agent_cpu_quota: env_parse("AGENT_CPU_QUOTA", 200_000)?,
            agent_cpu_period: env_parse("AGENT_CPU_PERIOD", 100_000)?,
            agent_shm_bytes: env_parse("AGENT_SHM_BYTES", 256 * 1024 * 1024)?,
            agent_pids_limit: env_parse("AGENT_PIDS_LIMIT", 256)?,
            agent_tmpfs_workspace_bytes: env_parse(
                "AGENT_TMPFS_WORKSPACE_BYTES",
                1 * 1024 * 1024 * 1024,
            )?,
            agent_tmpfs_tmp_bytes: env_parse("AGENT_TMPFS_TMP_BYTES", 512 * 1024 * 1024)?,
            agent_timeout_secs: env_parse("AGENT_TIMEOUT_SECS", 30 * 60)?,

            default_model: env_or("AGENT_DEFAULT_MODEL", "claude-sonnet-4-6"),
            default_effort: env_or("AGENT_DEFAULT_EFFORT", "high"),

            host_claude_credentials: resolve_creds_path(),
            claude_oauth_token: std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok(),

            host_axint_mcp_url: std::env::var("HOST_AXINT_MCP_URL")
                .ok()
                .filter(|s| !s.is_empty()),
            host_crabcc_dir: resolve_crabcc_dir(),
            agent_network: env_or("AGENT_NETWORK", "bridge"),

            stream_maxlen: env_parse("STREAM_MAXLEN", 10_000)?,
            stream_key_prefix: env_or("STREAM_KEY_PREFIX", "crabcc:agent:logs"),

            smoke: matches!(
                std::env::var("CRABCC_AGENT_SMOKE").as_deref(),
                Ok("1") | Ok("true") | Ok("yes")
            ),

            prewarm: !matches!(
                std::env::var("AGENTS_PREWARM").as_deref(),
                Ok("0") | Ok("false") | Ok("no")
            ),
            tokio_worker_threads: env_parse(
                "AGENTS_TOKIO_THREADS",
                std::thread::available_parallelism()
                    .map(|n| n.get().min(4))
                    .unwrap_or(2),
            )?,

            // LiteLLM URL — discovery resolves it; setting the env to
            // `-` / `none` opts out, in which case we treat it as
            // disabled (no preflight at boot).
            litellm_base_url: match std::env::var("LITELLM_BASE_URL").ok().as_deref() {
                Some("-") | Some("none") => None,
                _ => Some(litellm.url),
            },
            litellm_required: matches!(
                std::env::var("LITELLM_REQUIRED").as_deref(),
                Ok("1") | Ok("true") | Ok("yes")
            ),
            litellm_probe_timeout: Duration::from_millis(env_parse(
                "LITELLM_PROBE_TIMEOUT_MS",
                1500,
            )?),
        })
    }

    /// URL with the password masked for log output.
    pub fn redis_url_redacted(&self) -> String {
        match url_split_password(&self.redis_url) {
            Some((head, tail)) => format!("{head}***{tail}"),
            None => self.redis_url.clone(),
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> Result<T>
where
    <T as std::str::FromStr>::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(v) => v
            .parse::<T>()
            .map_err(|e| anyhow::anyhow!("env {key} = {v:?}: {e}")),
        Err(_) => Ok(default),
    }
}

/// Resolve the host-side `.crabcc/` directory. Honour
/// `HOST_CRABCC_DIR` first; fall back to `$HOME/.crabcc`. Empty
/// string opts out of the bind-mount entirely (e.g. running in
/// strict-isolation mode). The path is *not* required to exist at
/// boot — `Runner::host_config` checks existence per-job before
/// adding the mount, so a host that hasn't run `crabcc init` yet
/// still gets a working worker.
fn resolve_crabcc_dir() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("HOST_CRABCC_DIR") {
        if s.is_empty() {
            return None;
        }
        return Some(PathBuf::from(s));
    }
    let home = std::env::var("HOME").ok()?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".crabcc"))
}

fn resolve_creds_path() -> Option<PathBuf> {
    let raw = std::env::var("HOST_CLAUDE_CREDENTIALS").unwrap_or_default();
    if raw == "-" || raw.eq_ignore_ascii_case("none") {
        return None;
    }
    let path = if raw.is_empty() {
        let home = std::env::var("HOME").ok()?;
        PathBuf::from(home).join(".claude/.credentials.json")
    } else {
        PathBuf::from(raw)
    };
    // Don't fail boot on missing file — runner falls back to the OAuth
    // token env. The `health` probe will surface the situation instead.
    Some(path)
}

fn url_split_password(url: &str) -> Option<(String, String)> {
    let at = url.rfind('@')?;
    let scheme_end = url.find("://")? + 3;
    let creds = &url[scheme_end..at];
    let colon = creds.find(':')?;
    let head = &url[..scheme_end + colon + 1];
    let tail = &url[at..];
    Some((head.to_string(), tail.to_string()))
}
