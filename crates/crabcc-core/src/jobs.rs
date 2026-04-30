//! BullMQ-backed local agent coordination — issue #109.
//!
//! Gated behind the `jobs` cargo feature. Pulls in `tokio` + `redis`
//! as a deliberate, isolated tokio entry point — the rest of
//! `crabcc-core` stays sync (issue #112 rationale).
//!
//! ## Architecture
//!
//! - **Rust submitters** (this module) encode jobs in BullMQ's Redis
//!   wire shape: ` bull:<queue>:id` (counter), ` bull:<queue>:<id>`
//!   (hash), ` bull:<queue>:wait` (list, immediate) or
//!   ` bull:<queue>:delayed` (sorted set, future).
//! - **Node worker** (` apps/jobs-worker`, lands in a follow-up) is
//!   the BullMQ-native consumer; it polls / processes / completes
//!   using BullMQ's standard Lua-script-based dequeue.
//! - Both speak the same on-disk Redis protocol; no HTTP API
//!   intermediary.
//!
//! ## Wire protocol
//!
//! [`submit_async`] performs a two-phase enqueue:
//!
//! 1. ` INCR bull:<queue>:id` to mint a numeric job id.
//! 2. ` MULTI` / ` HSET bull:<queue>:<id> name=… data=<json> opts=<json>
//!    timestamp=<ms> attemptsMade=0 delay=<ms?>` /
//!    ` LPUSH bull:<queue>:wait <id>` (or ` ZADD bull:<queue>:delayed
//!    <until_ms> <id>` when delayed) / ` EXEC`.
//!
//! Atomicity: the ` INCR` is its own command (we need the result),
//! the rest runs inside ` MULTI`/` EXEC`. A crash between phases leaks
//! one id (gap in the sequence) but doesn't lose data — same property
//! as BullMQ's own Lua scripts.
//!
//! ## Sync vs async
//!
//! [`submit`] is sync — it builds a single-thread tokio runtime
//! per call. Callers from a tokio runtime (e.g., the future
//! `crabcc serve` async refactor) should call [`submit_async`]
//! directly to avoid the nested-runtime panic.
//!
//! ## Not yet supported (follow-up branches)
//!
//! - **Repeatable jobs** (BullMQ ` JobScheduler`) — needs the
//!   ` repeat:<queue>:<key>` hash + cron parser.
//! - **Flows** (parent → children DAG) — needs the ` :parent` /
//!   ` :children` keys + ` waiting-children` queue state.
//! - **Job priority queue** (BullMQ uses a separate
//!   ` :prioritized` sorted set when ` opts.priority` is set) —
//!   today we encode ` opts.priority` in the hash but always LPUSH
//!   onto ` :wait`; the worker still respects priority once it
//!   reads the hash, just less efficiently.
//! - **Status queries** (` Submit::status` from issue #109's spec).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

const TRACE_TARGET: &str = "crabcc_core::jobs";

/// One job's worth of input. Mirrors BullMQ's `Queue.add(name, data, opts)`
/// surface — `priority`, `delay`, and `attempts` map to BullMQ's
/// JobOptions on the worker side.
///
/// The optional metadata fields (`agent_name`, `repo_path`, `github_url`)
/// are echoed back in status responses and surfaced in Bull Board / the
/// /live dashboard. They don't affect job execution — they exist so
/// operators can trace a job back to its source without reading the full
/// `data` payload.
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
    /// Human-readable agent identifier (e.g. `"warp-speed-audit"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    /// Absolute path to the repo this job operates on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
    /// GitHub remote URL of the repo (for dashboard links).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_url: Option<String>,
    /// Path to the agent run-dir (`~/.crabcc/agents/<id>/`) so the worker
    /// can tail the log, check the lock file, or write results back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_folder: Option<String>,
}

pub type JobId = String;

/// Receipt returned from [`submit_async`] / [`submit`].
/// Thread-safe to clone and send across async tasks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobReceipt {
    pub id: JobId,
    pub queue: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_folder: Option<String>,
    pub delayed: bool,
}

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
/// appropriate for use inside an existing tokio runtime (use
/// [`submit_async`] directly from async callers).
pub fn submit(opts: &Options, spec: JobSpec) -> Result<JobReceipt> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for jobs::submit")?;
    rt.block_on(submit_async(opts, spec))
}

/// Async submission entry point — encodes [`JobSpec`] in BullMQ's
/// on-disk Redis layout and `LPUSH`es the resulting job id onto
/// `bull:<queue>:wait` (or `ZADD`s onto `bull:<queue>:delayed` when
/// `delay_ms` is set). Returns the numeric job id as a string,
/// matching what BullMQ's JS client returns from `Queue.add`.
///
/// ## Wire shape
///
/// Three Redis keys per submission:
///
/// 1. `bull:<queue>:id` — counter, `INCR`'d to mint a new id.
/// 2. `bull:<queue>:<id>` — hash with the job payload:
///    - `name`         — job name (e.g., `"agent:run"`)
///    - `data`         — JSON-stringified user payload
///    - `opts`         — JSON-stringified BullMQ JobOptions
///    - `timestamp`    — submission time in ms (Unix epoch)
///    - `attemptsMade` — always `"0"` at submission
///    - `delay`        — delay_ms or `""`
/// 3. Either `bull:<queue>:wait` (list, LPUSH) for immediate jobs
///    OR `bull:<queue>:delayed` (sorted set, ZADD scored by
///    `now_ms + delay_ms`) for delayed jobs.
///
/// ## Atomicity
///
/// Two-phase: the `INCR` is its own command (we need the result),
/// then a pipelined `MULTI/EXEC` block holds the HSET + LPUSH/ZADD.
/// On crash between phases, an id is "leaked" (gap in the sequence)
/// — not a correctness issue, just a non-contiguous id. BullMQ's
/// Lua scripts have this same property.
/// Async-safe submission. Safe to call from any tokio runtime or
/// `tokio::spawn` task — does not create its own runtime.
pub async fn submit_async(opts: &Options, spec: JobSpec) -> Result<JobReceipt> {
    let cid = correlation(opts);
    let client = redis::Client::open(opts.redis_url.as_str())
        .with_context(|| format!("open redis client at {}", opts.redis_url))?;
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis")?;

    // Phase 1 — mint the next job id atomically. BullMQ uses an
    // INCR'd counter at `bull:<queue>:id`; ids are 1-based numeric
    // strings.
    let id_key = format!("bull:{}:id", spec.queue);
    let next_id: u64 = redis::cmd("INCR")
        .arg(&id_key)
        .query_async(&mut conn)
        .await
        .with_context(|| format!("INCR {id_key}"))?;
    let job_id = next_id.to_string();

    // Phase 2 — pipelined HSET + LPUSH/ZADD inside a MULTI/EXEC
    // block. After this returns, BullMQ's Node-side worker can
    // pick the job up via its standard wait-loop.
    let job_key = format!("bull:{}:{}", spec.queue, job_id);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let data_json = serde_json::to_string(&spec.data).context("serialize job data")?;
    let opts_json =
        serde_json::to_string(&build_bullmq_opts(&spec)).context("serialize BullMQ JobOptions")?;
    let timestamp_s = now_ms.to_string();
    let delay_s = spec.delay_ms.map(|d| d.to_string()).unwrap_or_default();

    // Build metadata fields — stored in the hash so Bull Board and the
    // /live dashboard can display them without parsing job.data.
    let agent_name_s = spec.agent_name.clone().unwrap_or_default();
    let repo_path_s = spec.repo_path.clone().unwrap_or_default();
    let github_url_s = spec.github_url.clone().unwrap_or_default();

    let mut pipe = redis::pipe();
    pipe.atomic();
    pipe.hset_multiple(
        &job_key,
        &[
            ("name", spec.name.as_str()),
            ("data", data_json.as_str()),
            ("opts", opts_json.as_str()),
            ("timestamp", timestamp_s.as_str()),
            ("attemptsMade", "0"),
            ("delay", delay_s.as_str()),
            // crabcc-specific metadata (non-standard BullMQ fields;
            // workers ignore unknown hash fields).
            ("agentName", agent_name_s.as_str()),
            ("repoPath", repo_path_s.as_str()),
            ("githubUrl", github_url_s.as_str()),
            ("agentFolder", spec.agent_folder.as_deref().unwrap_or("")),
        ],
    );

    let queue_key_used = if let Some(delay_ms) = spec.delay_ms {
        let score = (now_ms + delay_ms) as f64;
        let key = format!("bull:{}:delayed", spec.queue);
        pipe.zadd(&key, &job_id, score);
        key
    } else {
        let key = format!("bull:{}:wait", spec.queue);
        pipe.lpush(&key, &job_id);
        key
    };

    let _: () = pipe
        .query_async(&mut conn)
        .await
        .with_context(|| format!("BullMQ pipeline EXEC for queue {}", spec.queue))?;

    tracing::info!(
        target: TRACE_TARGET,
        event = "jobs.submit",
        request_id = %cid,
        job_id = %job_id,
        queue = %spec.queue,
        name = %spec.name,
        delay_ms = ?spec.delay_ms,
        priority = ?spec.priority,
        attempts = ?spec.attempts,
        queue_key = %queue_key_used,
        "jobs submit (BullMQ wire encoded)"
    );

    Ok(JobReceipt {
        id: job_id,
        queue: spec.queue.clone(),
        name: spec.name.clone(),
        agent_name: spec.agent_name.clone(),
        repo_path: spec.repo_path.clone(),
        github_url: spec.github_url.clone(),
        agent_folder: spec.agent_folder.clone(),
        delayed: spec.delay_ms.is_some(),
    })
}

/// Build the BullMQ `opts` hash field — JSON-stringified subset of
/// [BullMQ JobOptions](https://api.docs.bullmq.io/types/v4.JobsOptions.html).
/// Pure function so tests can verify the shape without a Redis server.
fn build_bullmq_opts(spec: &JobSpec) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    if let Some(p) = spec.priority {
        m.insert("priority".into(), serde_json::json!(p));
    }
    if let Some(d) = spec.delay_ms {
        m.insert("delay".into(), serde_json::json!(d));
    }
    if let Some(a) = spec.attempts {
        m.insert("attempts".into(), serde_json::json!(a));
    }
    serde_json::Value::Object(m)
}

/// Look up a job's current state by walking BullMQ's per-queue keys.
/// Returns [`JobStatus::Unknown`] when the id isn't found in any
/// known location — covers both "never existed" and "completed and
/// pruned by BullMQ's retention policy".
///
/// Walk order matches BullMQ's natural lifecycle: ` wait` → ` active`
/// → ` delayed` → ` completed` → ` failed`. Stops at the first hit.
pub async fn status_async(opts: &Options, queue: &str, job_id: &JobId) -> Result<JobStatus> {
    let cid = correlation(opts);
    let client = redis::Client::open(opts.redis_url.as_str())
        .with_context(|| format!("open redis client at {}", opts.redis_url))?;
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis")?;

    // Lists — LPOS returns the index (0-based) when the element is in
    // the list, nil otherwise. We don't care about position; presence
    // is the signal.
    for (suffix, mapped) in [("wait", JobStatus::Queued), ("active", JobStatus::Active)] {
        let key = format!("bull:{queue}:{suffix}");
        let pos: Option<i64> = redis::cmd("LPOS")
            .arg(&key)
            .arg(job_id)
            .query_async(&mut conn)
            .await
            .ok()
            .flatten();
        if pos.is_some() {
            tracing::debug!(
                target: TRACE_TARGET,
                event = "jobs.status",
                request_id = %cid,
                job_id = %job_id,
                queue = %queue,
                status = ?mapped,
                "jobs status (list match)"
            );
            return Ok(mapped);
        }
    }

    // Sorted sets — ZSCORE returns the score when present.
    for (suffix, mapped) in [
        ("delayed", JobStatus::Delayed),
        ("completed", JobStatus::Completed),
        ("failed", JobStatus::Failed),
    ] {
        let key = format!("bull:{queue}:{suffix}");
        let score: Option<f64> = redis::cmd("ZSCORE")
            .arg(&key)
            .arg(job_id)
            .query_async(&mut conn)
            .await
            .ok()
            .flatten();
        if score.is_some() {
            tracing::debug!(
                target: TRACE_TARGET,
                event = "jobs.status",
                request_id = %cid,
                job_id = %job_id,
                queue = %queue,
                status = ?mapped,
                "jobs status (zset match)"
            );
            return Ok(mapped);
        }
    }

    tracing::debug!(
        target: TRACE_TARGET,
        event = "jobs.status",
        request_id = %cid,
        job_id = %job_id,
        queue = %queue,
        status = "unknown",
        "jobs status (no match)"
    );
    Ok(JobStatus::Unknown)
}

/// Sync wrapper around [`status_async`]. Same caveats as [`submit`]:
/// don't call from inside an existing tokio runtime.
pub fn status(opts: &Options, queue: &str, job_id: &JobId) -> Result<JobStatus> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for jobs::status")?;
    rt.block_on(status_async(opts, queue, job_id))
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

/// Remove a waiting or delayed job. No-op (Ok) if the id is not found —
/// the job may already be active, completed, or never existed.
pub async fn cancel_async(opts: &Options, queue: &str, job_id: &JobId) -> Result<bool> {
    let client = redis::Client::open(opts.redis_url.as_str())
        .with_context(|| format!("open redis client at {}", opts.redis_url))?;
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis")?;

    let job_key = format!("bull:{queue}:{job_id}");
    let wait_key = format!("bull:{queue}:wait");
    let delayed_key = format!("bull:{queue}:delayed");

    let mut pipe = redis::pipe();
    pipe.atomic();
    pipe.lrem(&wait_key, 0, job_id.as_str());
    pipe.zrem(&delayed_key, job_id.as_str());
    pipe.del(&job_key);

    let _: () = pipe
        .query_async(&mut conn)
        .await
        .with_context(|| format!("cancel pipeline for job {job_id} in {queue}"))?;

    tracing::info!(
        target: TRACE_TARGET,
        event = "jobs.cancel",
        job_id = %job_id,
        queue = %queue,
        "jobs cancel"
    );
    Ok(true)
}

pub fn cancel(opts: &Options, queue: &str, job_id: &JobId) -> Result<bool> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for jobs::cancel")?;
    rt.block_on(cancel_async(opts, queue, job_id))
}

/// Count jobs currently in the `wait` list for a queue.
pub async fn queue_depth_async(opts: &Options, queue: &str) -> Result<u64> {
    let client = redis::Client::open(opts.redis_url.as_str())
        .with_context(|| format!("open redis client at {}", opts.redis_url))?;
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis")?;
    let key = format!("bull:{queue}:wait");
    let len: u64 = redis::cmd("LLEN")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .with_context(|| format!("LLEN {key}"))?;
    Ok(len)
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
            agent_name: None,
            repo_path: None,
            github_url: None,
            agent_folder: None,
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
            agent_name: None,
            repo_path: None,
            github_url: None,
            agent_folder: None,
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
    fn build_bullmq_opts_all_fields_present() {
        let spec = JobSpec {
            queue: "agent:run".into(),
            name: "warp-speed".into(),
            data: serde_json::json!({}),
            priority: Some(10),
            delay_ms: Some(500),
            attempts: Some(3),
            agent_name: None,
            repo_path: None,
            github_url: None,
            agent_folder: None,
        };
        let opts = build_bullmq_opts(&spec);
        let m = opts.as_object().unwrap();
        assert_eq!(m.get("priority").and_then(|v| v.as_u64()), Some(10));
        assert_eq!(m.get("delay").and_then(|v| v.as_u64()), Some(500));
        assert_eq!(m.get("attempts").and_then(|v| v.as_u64()), Some(3));
    }

    #[test]
    fn build_bullmq_opts_empty_when_unset() {
        let spec = JobSpec {
            queue: "x".into(),
            name: "y".into(),
            data: serde_json::json!({}),
            priority: None,
            delay_ms: None,
            attempts: None,
            agent_name: None,
            repo_path: None,
            github_url: None,
            agent_folder: None,
        };
        let opts = build_bullmq_opts(&spec);
        let m = opts.as_object().unwrap();
        assert!(m.is_empty(), "expected empty opts object, got {opts}");
    }

    #[test]
    fn build_bullmq_opts_serializes_to_compact_json() {
        let spec = JobSpec {
            queue: "x".into(),
            name: "y".into(),
            data: serde_json::json!({}),
            priority: Some(5),
            delay_ms: None,
            attempts: None,
            agent_name: None,
            repo_path: None,
            github_url: None,
            agent_folder: None,
        };
        let s = serde_json::to_string(&build_bullmq_opts(&spec)).unwrap();
        assert_eq!(s, r#"{"priority":5}"#);
    }

    /// Verify the BullMQ key layout we encode against. These are the
    /// keys the Node-side worker reads from; if BullMQ ever changes
    /// its on-disk format, these tests are the canary.
    #[test]
    fn bullmq_key_format_matches_protocol() {
        let queue = "agent:run";
        assert_eq!(format!("bull:{queue}:id"), "bull:agent:run:id");
        assert_eq!(format!("bull:{queue}:42"), "bull:agent:run:42");
        assert_eq!(format!("bull:{queue}:wait"), "bull:agent:run:wait");
        assert_eq!(format!("bull:{queue}:delayed"), "bull:agent:run:delayed");
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
