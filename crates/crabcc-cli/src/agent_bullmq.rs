//! BullMQ-backed [`AgentRuntime`] — enqueues the run on the
//! `crabcc-agents` worker (sibling crate at `apps/crabcc-agents/`) and
//! tails the per-job Redis Stream back into this run's log file. The
//! caller-visible contract matches `SubprocessRuntime`: the agent's
//! stdout/stderr lands in `~/.crabcc/agents/<id>/log` and the function
//! returns the exit code, so `crabcc agent ls` / `kill` / log tailing
//! all keep working.
//!
//! Behind the `agents-bullmq` Cargo feature; vanilla CLI builds skip
//! the bullmq-rs + redis dep cost entirely.
//!
//! Env contract — same defaults as the worker, so no extra wiring is
//! needed when both ship together:
//!
//!   REDIS_URL          redis://127.0.0.1:6379
//!   AGENTS_QUEUE       crabcc:agents
//!   STREAM_KEY_PREFIX  crabcc:agent:logs
//!   AGENT_DEFAULT_MODEL claude-sonnet-4-6
//!   AGENT_DEFAULT_EFFORT high

#![cfg(feature = "agents-bullmq")]

use ahash::HashMap;
use anyhow::{Context, Result};
use bullmq_rs::{QueueBuilder, RedisConnection};
use futures_util::TryFutureExt;
use redis::aio::ConnectionManager;
use redis::streams::{StreamReadOptions, StreamReadReply};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;

use crate::agent::{AgentRequest, AgentRuntime, RunDir};

/// Mirrors `apps/crabcc-agents/src/job.rs::AgentJob`. We re-declare it
/// here rather than depend on the standalone crate so the CLI doesn't
/// pull the worker's runtime dep tree (bollard, etc.).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
enum AgentKind {
    #[default]
    ClaudeCode,
    MiniSwe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentJob {
    prompt: String,
    #[serde(default)]
    kind: AgentKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(default)]
    sandbox: SandboxSpec,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    env: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SandboxSpec {
    network: bool,
    writeable_root: bool,
    bash: bool,
}
impl Default for SandboxSpec {
    fn default() -> Self {
        Self {
            network: false,
            writeable_root: false,
            bash: true,
        }
    }
}

pub struct BullmqRuntime;

impl AgentRuntime for BullmqRuntime {
    fn label(&self) -> &'static str {
        "bullmq (sandboxed Docker via crabcc-agents)"
    }

    fn run(&self, req: &AgentRequest<'_>, run: &RunDir) -> Result<i32> {
        if req.dry_run {
            print_dry_run(req, run);
            return Ok(0);
        }

        // The CLI's `run()` is a sync entry point. Spin up a small
        // dedicated tokio runtime here rather than thread async all the
        // way up — keeps the AgentRuntime trait sync-shaped for parity
        // with SubprocessRuntime + sandbox stub.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("tokio runtime for bullmq agent")?;
        rt.block_on(run_async(req, run))
    }
}

fn print_dry_run(req: &AgentRequest<'_>, run: &RunDir) {
    println!("crabcc agent — dry run (transport=bullmq)");
    println!("  run id          : {}", run.id);
    println!("  log (tail -f)   : {}", run.log_path.display());
    println!(
        "  redis           : {}",
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into())
    );
    println!(
        "  queue           : {}",
        std::env::var("AGENTS_QUEUE").unwrap_or_else(|_| "crabcc:agents".into())
    );
    let preview: String = req.prompt.chars().take(160).collect();
    println!("  prompt          : {} chars", req.prompt.chars().count());
    println!("  preview         : {preview}");
    println!("(no enqueue — re-run without --dry-run to dispatch)");
}

async fn run_async(req: &AgentRequest<'_>, run: &RunDir) -> Result<i32> {
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    let queue_name = std::env::var("AGENTS_QUEUE").unwrap_or_else(|_| "crabcc:agents".into());
    let stream_prefix =
        std::env::var("STREAM_KEY_PREFIX").unwrap_or_else(|_| "crabcc:agent:logs".into());

    // ── enqueue ────────────────────────────────────────────────────
    let conn = RedisConnection::new(&redis_url);
    let queue = QueueBuilder::new(&queue_name)
        .connection(conn)
        .build::<AgentJob>()
        .await
        .with_context(|| format!("connect bullmq queue {queue_name} on {redis_url}"))?;

    // Default trackability headers — caller-supplied via CRABCC_HEADER_*
    // override; we always set x-source=cli so consumers can route by
    // origin without producers having to remember.
    let mut headers = collect_headers_from_env();
    headers
        .entry("x-source".into())
        .or_insert_with(|| "cli".into());
    headers
        .entry("x-job-run-id".into())
        .or_insert_with(|| run.id.clone());

    // ── Container context — forward the bits an agent needs to do
    // its job. The worker's `compose_env` (apps/crabcc-agents/src/
    // runner.rs) translates `payload.env` to container env vars
    // verbatim, so populating it here is enough to reach the agent
    // process inside the sandbox.
    //
    // - `CRABCC_ROOT`: host-side path to the project the agent is
    //   meant to operate on. The container has no host fs by
    //   default; this is informational ("which repo am I working
    //   on?") and is consumed by the entrypoint when bind-mounting
    //   the project into /workspace lands as a follow-up.
    //
    // - `OLLAMA_BASE_URL` / `LITELLM_BASE_URL`: when the host runs
    //   the local Ollama stack (`install/ollama-stack/`), agents
    //   should hit it instead of cloud Anthropic. Forwarded as-is;
    //   the agent rewrites `127.0.0.1` to `host.docker.internal`
    //   itself if the value isn't already host-routable. Absent on
    //   the host = absent in the container (agent falls back to
    //   whatever its own discovery does).
    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("CRABCC_ROOT".into(), req.root.display().to_string());
    if let Ok(url) = std::env::var("OLLAMA_BASE_URL") {
        if !url.is_empty() {
            env.insert("OLLAMA_BASE_URL".into(), url);
        }
    }
    if let Ok(url) = std::env::var("LITELLM_BASE_URL") {
        if !url.is_empty() {
            env.insert("LITELLM_BASE_URL".into(), url);
        }
    }

    let job = AgentJob {
        prompt: req.prompt.to_string(),
        // CRABCC_AGENT_KIND mirrors the worker's AGENT_KIND on the
        // producer side. Default = claude-code.
        kind: match std::env::var("CRABCC_AGENT_KIND").ok().as_deref() {
            Some("mini-swe") | Some("mini") => AgentKind::MiniSwe,
            _ => AgentKind::ClaudeCode,
        },
        model: req.model.clone(),
        effort: std::env::var("AGENT_DEFAULT_EFFORT").ok(),
        sandbox: SandboxSpec::default(),
        env,
        timeout_secs: None,
        headers,
    };
    let queued = queue
        .add(&run.id, job, None)
        .await
        .context("enqueue bullmq job")?;

    eprintln!(
        "crabcc agent: enqueued bullmq job id={} (run.id={}, stream={}:{})",
        queued.id, run.id, stream_prefix, queued.id
    );

    // ── tail stream ────────────────────────────────────────────────
    let stream_key = format!("{stream_prefix}:{}", queued.id);
    let log_path = run.log_path.clone();
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open log file {}", log_path.display()))?;

    let client = redis::Client::open(redis_url.as_str()).context("redis client")?;
    let mut redis_conn = ConnectionManager::new(client)
        .map_err(|e| anyhow::anyhow!(e))
        .await
        .context("redis connection manager")?;

    let mut last_id = "0-0".to_string();
    let opts = StreamReadOptions::default().block(0);

    loop {
        let reply: StreamReadReply = redis::AsyncCommands::xread_options(
            &mut redis_conn,
            &[&stream_key],
            &[&last_id],
            &opts,
        )
        .await
        .with_context(|| format!("xread {stream_key} from {last_id}"))?;

        for stream in reply.keys {
            for entry in stream.ids {
                last_id = entry.id.clone();
                let source = entry
                    .map
                    .get("s")
                    .and_then(redis_value_to_string)
                    .unwrap_or_default();
                let message = entry
                    .map
                    .get("m")
                    .and_then(redis_value_to_string)
                    .unwrap_or_default();

                // Tee to log file + parent's stdout/stderr.
                let line = format!("[{source}] {message}\n");
                let _ = log.write_all(line.as_bytes());
                match source.as_str() {
                    "stderr" => eprint!("{line}"),
                    _ => print!("{line}"),
                }

                // Sentinel: `__eof__ exit=N`. Worker writes this on
                // container exit; we parse N and return it.
                if source == "event" {
                    if let Some(rest) = message.strip_prefix("__eof__ exit=") {
                        let exit_code: i32 = rest.trim().parse().unwrap_or(-1);
                        return Ok(exit_code);
                    }
                }
            }
        }
    }
}

/// Same convention as `apps/crabcc-agents/src/bin/seed.rs`: scoop the
/// `CRABCC_HEADER_*` env into a lower-case-`-`-keyed map.
fn collect_headers_from_env() -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (k, v) in std::env::vars() {
        if let Some(rest) = k.strip_prefix("CRABCC_HEADER_") {
            out.insert(rest.to_lowercase().replace('_', "-"), v);
        }
    }
    out
}

fn redis_value_to_string(v: &redis::Value) -> Option<String> {
    match v {
        redis::Value::BulkString(b) => String::from_utf8(b.clone()).ok(),
        redis::Value::SimpleString(s) => Some(s.clone()),
        _ => None,
    }
}
