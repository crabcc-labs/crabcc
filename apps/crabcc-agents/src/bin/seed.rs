//! Test producer — pushes one synthetic AgentJob onto the queue.
//!
//! Usage:
//!   cargo run --bin seed -- "say hi"
//!
//! Honours the same env vars as the worker (REDIS_URL, AGENTS_QUEUE).

use anyhow::Result;
use bullmq_rs::{QueueBuilder, RedisConnection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SandboxSpec {
    #[serde(default)]
    network: bool,
    #[serde(default)]
    writeable_root: bool,
    #[serde(default = "yes")]
    bash: bool,
}
fn yes() -> bool {
    true
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
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    sandbox: SandboxSpec,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    headers: HashMap<String, String>,
}

/// Scoop CRABCC_HEADER_* env vars into a header map. The producer sets
/// these before invoking seed (or the bullmq runtime); we forward them
/// onto the BullMQ job so the worker can propagate them downstream.
///
///   CRABCC_HEADER_X_SOURCE=live-web → headers["x-source"] = "live-web"
fn collect_headers_from_env() -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (k, v) in std::env::vars() {
        if let Some(rest) = k.strip_prefix("CRABCC_HEADER_") {
            let key = rest.to_lowercase().replace('_', "-");
            out.insert(key, v);
        }
    }
    out
}

#[tokio::main]
async fn main() -> Result<()> {
    let prompt = std::env::args().nth(1).unwrap_or_else(|| "say hi".into());
    let kind = match std::env::var("AGENT_KIND").ok().as_deref() {
        Some("mini-swe") | Some("mini") => AgentKind::MiniSwe,
        _ => AgentKind::ClaudeCode,
    };
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    let queue_name = std::env::var("AGENTS_QUEUE").unwrap_or_else(|_| "crabcc:agents".into());

    let conn = RedisConnection::new(&redis_url);
    let queue = QueueBuilder::new(&queue_name)
        .connection(conn)
        .build::<AgentJob>()
        .await?;

    let headers = collect_headers_from_env();
    let job = AgentJob {
        prompt: prompt.clone(),
        kind,
        model: None,
        effort: None,
        sandbox: SandboxSpec::default(),
        env: HashMap::new(),
        timeout_secs: Some(30),
        headers,
    };

    let queued = queue.add("smoke", job, None).await?;
    println!(
        "{}",
        serde_json::json!({
            "ok": true,
            "id": queued.id,
            "queue": queue_name,
            "prompt": prompt,
            "stream_key": format!("crabcc:agent:logs:{}", queued.id),
        })
    );
    Ok(())
}
