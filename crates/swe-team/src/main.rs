//! swe-team — a multi-agent software-engineering team on top of rig.
//!
//! Phase-1 core graph (LangGraph-style, explicit nodes + capped loops):
//!   PLAN -> LEAD GATE -> FANOUT (3 coders ‖) -> SYNTH -> REVIEW -> SELF-REVIEW
//!   -> EMIT.
//! The Lead Dev gates the plan (APPROVE/REVISE/STOP), and on APPROVE both
//! pre-configures the coders' model params and pre-injects crabcc context.
//! The final diff is printed, never auto-applied unless `--apply` is given.
//! Each node emits a JSONL `TraceEvent` to stdout — the Phase-2 observability
//! seam (see `trace.rs`).

mod agents;
mod graph;
mod lead;
mod tools;
mod trace;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use clap::Parser;
use rig::providers::openai;

use graph::{GraphState, Models};

#[derive(Parser)]
#[command(
    name = "swe-team",
    about = "Multi-agent SWE team: plan -> lead gate -> 3 parallel coders -> synth -> review -> emit"
)]
struct Cli {
    /// Path to the target repository (read-only; tools run with this as cwd).
    #[arg(long)]
    repo: PathBuf,

    /// Apply the final unified diff to the repo with `git apply` (default: only
    /// print it).
    #[arg(long)]
    apply: bool,

    /// The engineering task to plan and implement.
    task: String,
}

/// Resolve a value from the first set env var in `keys`, else `default`.
fn env_or(keys: &[&str], default: &str) -> String {
    for k in keys {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                return v;
            }
        }
    }
    default.to_string()
}

/// Build one Chat-Completions client pointed at the gateway. rig's default
/// `openai::Client` speaks the Responses API; gateways (LiteLLM / Ollama)
/// expose Chat Completions at `/v1/chat/completions`, so we convert via
/// `.completions_api()`.
fn build_client() -> Result<openai::CompletionsClient> {
    let base_url = env_or(
        &["SWE_GATEWAY_URL", "OLLAMA_BASE_URL"],
        "http://localhost:4000/v1",
    );
    let api_key = env_or(&["LITELLM_MASTER_KEY", "OLLAMA_API_KEY"], "sk-noauth");

    let client = openai::Client::builder()
        .api_key(api_key)
        .base_url(&base_url)
        .build()
        .with_context(|| format!("building rig openai client for gateway {base_url}"))?;

    Ok(client.completions_api())
}

/// `git apply` the diff with cwd = repo, feeding it on stdin.
fn git_apply(repo: &std::path::Path, diff: &str) -> Result<()> {
    let mut child = Command::new("git")
        .arg("apply")
        .current_dir(repo)
        .stdin(Stdio::piped())
        .spawn()
        .context("spawning `git apply`")?;
    child
        .stdin
        .take()
        .context("git apply stdin unavailable")?
        .write_all(diff.as_bytes())
        .context("writing diff to git apply")?;
    let status = child.wait().context("waiting on git apply")?;
    if !status.success() {
        bail!("git apply failed (status {status}); the diff was not applied");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let repo = cli
        .repo
        .canonicalize()
        .with_context(|| format!("--repo path does not exist: {}", cli.repo.display()))?;

    let models = Models {
        coder: env_or(&["SWE_CODER_MODEL"], "qwen2.5-coder"),
        lead: env_or(&["SWE_LEAD_MODEL"], "claude-sonnet-4-6"),
        synth: env_or(&["SWE_SYNTH_MODEL"], "claude-sonnet-4-6"),
        review: env_or(&["SWE_REVIEW_MODEL"], "deepseek-v4-flash"),
    };

    let client = build_client()?;

    let outcome = GraphState::new(&client, &models, &repo, &cli.task)
        .run()
        .await?;

    if outcome.stopped {
        eprintln!("[swe-team] Lead Dev hard-stopped the plan; no diff produced.");
        return Ok(());
    }

    if cli.apply {
        git_apply(&repo, &outcome.final_diff)?;
        eprintln!("[swe-team] diff applied with `git apply`.");
    }

    Ok(())
}
