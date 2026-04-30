//! BullMQ-backed local agent coordination — issue #109.
//!
//! Gated behind the `jobs` cargo feature. Pulls in `tokio` + `redis`
//! as a deliberate, isolated tokio entry point — the rest of
//! `crabcc-core` stays sync (issue #112 rationale).
//!
//! ## Architecture
//!
//! - **Rust submitters** (this module) encode jobs in BullMQ's Redis
//!   wire shape (list + hash + sorted-set keys) and `LPUSH` onto
//!   `bull:<queue>:wait`.
//! - **Node worker** (`apps/jobs-worker`, lands in a follow-up) is
//!   the BullMQ-native consumer; it polls / processes / completes.
//! - Both speak the same on-disk Redis protocol; no HTTP API
//!   intermediary.
//!
//! ## Today (scaffold)
//!
//! [`submit`] establishes a connection and `PING`s Redis to prove
//! reachability. Actual BullMQ wire-protocol encoding (queue keys,
//! job hash, delayed-set ZADD, dependency map, JobScheduler
//! repeatable jobs) is a follow-up commit on this branch — issue
//! #109 acceptance criteria item 4.
//!
//! ## Sync vs async
//!
//! [`submit`] is sync — it builds a single-thread tokio runtime
//! per call. Callers from a tokio runtime (e.g., the future
//! `crabcc serve` async refactor) should call [`submit_async`]
//! directly to avoid the nested-runtime panic.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const TRACE_TARGET: &str = "crabcc_core::jobs";

/// One job's worth of input. Mirrors BullMQ's `Queue.add(name, data, opts)`
/// surface — `priority`, `delay`, and `attempts` map to BullMQ's
/// JobOptions on the worker side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub queue: String,
    pub name: String,
    pub data: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,
}

pub type JobId = String;

/// Status mirrors BullMQ's job lifecycle. `Unknown` is the catch-all
/// for protocol-version mismatches between the Rust submitter and
/// the Node worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Active,
    Completed,
    Failed,
    Delayed,
    Paused,
    Unknown,
}

/// Caller-provided knobs. [`Options::default`] reads `REDIS_URL` from
/// the env, falling back to localhost.
#[derive(Debug, Clone)]
pub struct Options {
    pub redis_url: String,
    pub correlation_id: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            redis_url: std::env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".into()),
            correlation_id: None,
        }
    }
}

impl Options {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_redis_url<S: Into<String>>(mut self, url: S) -> Self {
        self.redis_url = url.into();
        self
    }
    pub fn with_correlation_id<S: Into<String>>(mut self, id: S) -> Self {
        self.correlation_id = Some(id.into());
        self
    }
}

// ---------------------------------------------------------------------
// public surface
// ---------------------------------------------------------------------

/// Submit a job. Sync wrapper that spawns a current-thread tokio
/// runtime per call — fine for the CLI submission path; not
/// appropriate for use inside an existing tokio runtime.
pub fn submit(opts: &Options, spec: JobSpec) -> Result<JobId> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for jobs::submit")?;
    rt.block_on(submit_async(opts, spec))
}

/// Async submission entry point. Establishes a multiplexed Redis
/// connection and verifies it via `PING`. Today returns a synthetic
/// `JobId`; full BullMQ wire-protocol encoding is the next commit
/// on this scaffold (issue #109 AC #4).
pub async fn submit_async(opts: &Options, spec: JobSpec) -> Result<JobId> {
    let cid = correlation(opts);
    let client = redis::Client::open(opts.redis_url.as_str())
        .with_context(|| format!("open redis client at {}", opts.redis_url))?;
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis")?;

    // Sanity ping — fails fast on auth / network errors.
    let pong: String = redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .context("redis PING")?;
    if pong != "PONG" {
        return Err(anyhow::anyhow!(
            "redis PING returned unexpected response: {pong}"
        ));
    }

    // Synthetic id until the BullMQ wire-format encoder lands.
    let job_id = synth_job_id();

    tracing::info!(
        target: TRACE_TARGET,
        event = "jobs.submit",
        request_id = %cid,
        job_id = %job_id,
        queue = %spec.queue,
        name = %spec.name,
        delay_ms = ?spec.delay_ms,
        priority = ?spec.priority,
        "jobs submit (scaffold — Redis PING only, BullMQ encoding pending)"
    );

    Ok(job_id)
}

/// Probe Redis reachability. Useful for `crabcc doctor jobs` (issue
/// #107 + #109). Returns `Ok(())` only when PING returns PONG.
pub async fn ping_async(opts: &Options) -> Result<()> {
    let client = redis::Client::open(opts.redis_url.as_str())
        .with_context(|| format!("open redis client at {}", opts.redis_url))?;
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis")?;
    let pong: String = redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .context("redis PING")?;
    if pong != "PONG" {
        return Err(anyhow::anyhow!(
            "redis PING returned unexpected response: {pong}"
        ));
    }
    Ok(())
}

pub fn ping(opts: &Options) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for jobs::ping")?;
    rt.block_on(ping_async(opts))
}

// ---------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------

fn synth_job_id() -> JobId {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("ccc-job-{nanos:x}")
}

fn correlation(opts: &Options) -> String {
    opts.correlation_id
        .clone()
        .unwrap_or_else(|| format!("jobs-{}", synth_job_id()))
}

// ---------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Env-var tests share global state — run them sequentially in a
    // single test to avoid the cargo-test parallel-thread race.
    #[test]
    fn options_default_env_resolution() {
        let prev = std::env::var_os("REDIS_URL");

        // Case 1 — REDIS_URL set: should be used as-is.
        std::env::set_var("REDIS_URL", "redis://test:6380");
        assert_eq!(Options::default().redis_url, "redis://test:6380");

        // Case 2 — REDIS_URL absent: localhost fallback.
        std::env::remove_var("REDIS_URL");
        assert_eq!(Options::default().redis_url, "redis://127.0.0.1:6379");

        if let Some(p) = prev {
            std::env::set_var("REDIS_URL", p);
        }
    }

    #[test]
    fn job_spec_serializes_with_optional_fields_skipped() {
        let s = JobSpec {
            queue: "agent:run".into(),
            name: "warp-speed".into(),
            data: serde_json::json!({ "prompt": "ping" }),
            priority: None,
            delay_ms: None,
            attempts: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("priority"));
        assert!(!json.contains("delay_ms"));
        assert!(!json.contains("attempts"));
        assert!(json.contains("agent:run"));
    }

    #[test]
    fn job_spec_serializes_with_optional_fields_present() {
        let s = JobSpec {
            queue: "agent:flow".into(),
            name: "audit".into(),
            data: serde_json::json!({}),
            priority: Some(10),
            delay_ms: Some(500),
            attempts: Some(3),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"priority\":10"));
        assert!(json.contains("\"delay_ms\":500"));
        assert!(json.contains("\"attempts\":3"));
    }

    #[test]
    fn synth_job_id_starts_with_prefix() {
        let id = synth_job_id();
        assert!(id.starts_with("ccc-job-"));
        assert!(id.len() > "ccc-job-".len());
    }

    #[test]
    fn job_status_round_trips() {
        for s in [
            JobStatus::Queued,
            JobStatus::Active,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Delayed,
            JobStatus::Paused,
            JobStatus::Unknown,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: JobStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn options_builder_methods() {
        let opts = Options::new()
            .with_redis_url("redis://example:1234")
            .with_correlation_id("test-cid");
        assert_eq!(opts.redis_url, "redis://example:1234");
        assert_eq!(opts.correlation_id.as_deref(), Some("test-cid"));
    }
}
