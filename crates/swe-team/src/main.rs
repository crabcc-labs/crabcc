//! swe-team — a multi-agent software-engineering team on top of rig.
//!
//! Pipeline: Planner drafts a plan -> Lead Dev gates it (APPROVE/REVISE, up to
//! 3 rounds) -> on approval, 3 Coders (safety / perf / simplicity lenses) run
//! in parallel and each emits a candidate unified diff -> Synthesizer
//! reconciles them into one best-of-three diff. Output is printed; nothing is
//! applied to the repo.

mod agents;
mod tools;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use rig::completion::Prompt;
use rig::providers::openai;

use agents::{CoderLens, MAX_TURNS};

/// Cap on Planner <-> Lead Dev review rounds before proceeding with a warning.
const MAX_REVIEW_ROUNDS: usize = 3;

#[derive(Parser)]
#[command(
    name = "swe-team",
    about = "Multi-agent SWE team: plan -> review -> 3 parallel coders -> synthesize a diff"
)]
struct Cli {
    /// Path to the target repository (read-only; tools run with this as cwd).
    #[arg(long)]
    repo: PathBuf,

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
    let base_url = env_or(&["SWE_GATEWAY_URL", "OLLAMA_BASE_URL"], "http://localhost:4000/v1");
    let api_key = env_or(&["LITELLM_MASTER_KEY", "OLLAMA_API_KEY"], "sk-noauth");

    let client = openai::Client::builder()
        .api_key(api_key)
        .base_url(&base_url)
        .build()
        .with_context(|| format!("building rig openai client for gateway {base_url}"))?;

    Ok(client.completions_api())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let repo = cli
        .repo
        .canonicalize()
        .with_context(|| format!("--repo path does not exist: {}", cli.repo.display()))?;

    let coder_model = env_or(&["SWE_CODER_MODEL"], "qwen2.5-coder");
    let lead_model = env_or(&["SWE_LEAD_MODEL"], "claude-sonnet-4-6");
    let synth_model = env_or(&["SWE_SYNTH_MODEL"], "claude-sonnet-4-6");

    let client = build_client()?;

    // ---- 1. Planner drafts a plan -------------------------------------
    let planner = agents::planner(&client, &coder_model, &repo);
    let plan_prompt = format!(
        "Task:\n{}\n\nDraft an implementation plan for this task in the repo.",
        cli.task
    );
    let mut plan = planner
        .prompt(plan_prompt)
        .max_turns(MAX_TURNS)
        .await
        .context("planner failed")?;

    // ---- 2. Lead Dev review gate (APPROVE / REVISE, capped) -----------
    let lead = agents::lead_dev(&client, &lead_model);
    let mut approved = false;
    for round in 1..=MAX_REVIEW_ROUNDS {
        let review = lead
            .prompt(format!(
                "Task:\n{}\n\nProposed plan:\n{}\n\nReview it.",
                cli.task, plan
            ))
            .await
            .context("lead dev review failed")?;

        if review.trim_start().to_uppercase().starts_with("APPROVE") {
            approved = true;
            break;
        }

        eprintln!("[lead-dev] round {round}: REVISE");
        if round == MAX_REVIEW_ROUNDS {
            break;
        }

        // Feed the review notes back to the Planner for a revision.
        plan = planner
            .prompt(format!(
                "Task:\n{}\n\nYour previous plan:\n{}\n\nThe Lead Dev asked for these \
                 revisions:\n{}\n\nReturn the revised plan.",
                cli.task, plan, review
            ))
            .max_turns(MAX_TURNS)
            .await
            .context("planner revision failed")?;
    }

    if !approved {
        eprintln!(
            "[warning] plan not approved after {MAX_REVIEW_ROUNDS} rounds; proceeding with the \
             last plan"
        );
    }

    // ---- 3. Three coders in parallel, one per lens --------------------
    let safety = agents::coder(&client, &coder_model, &repo, &CoderLens::Safety);
    let perf = agents::coder(&client, &coder_model, &repo, &CoderLens::Perf);
    let simplicity = agents::coder(&client, &coder_model, &repo, &CoderLens::Simplicity);

    let coder_prompt = format!(
        "Task:\n{}\n\nApproved plan:\n{}\n\nImplement it. Output ONLY a unified diff.",
        cli.task, plan
    );

    let (safety_diff, perf_diff, simplicity_diff) = tokio::join!(
        safety.prompt(coder_prompt.clone()).max_turns(MAX_TURNS),
        perf.prompt(coder_prompt.clone()).max_turns(MAX_TURNS),
        simplicity.prompt(coder_prompt.clone()).max_turns(MAX_TURNS),
    );
    let safety_diff = safety_diff.context("safety coder failed")?;
    let perf_diff = perf_diff.context("perf coder failed")?;
    let simplicity_diff = simplicity_diff.context("simplicity coder failed")?;

    // ---- 4. Synthesize one best-of-three diff -------------------------
    let synth = agents::synthesizer(&client, &synth_model);
    let final_diff = synth
        .prompt(format!(
            "Task:\n{}\n\nApproved plan:\n{}\n\n\
             Candidate diff (safety lens):\n{}\n\n\
             Candidate diff (perf lens):\n{}\n\n\
             Candidate diff (simplicity lens):\n{}\n\n\
             Reconcile into ONE best-of-three unified diff.",
            cli.task, plan, safety_diff, perf_diff, simplicity_diff
        ))
        .await
        .context("synthesizer failed")?;

    // ---- Output ------------------------------------------------------
    println!("===== APPROVED PLAN =====\n{plan}\n");
    println!("===== SYNTHESIZED DIFF =====\n{final_diff}\n");
    println!("===== CANDIDATE DIFFS (for transparency) =====");
    println!("--- safety ---\n{safety_diff}\n");
    println!("--- perf ---\n{perf_diff}\n");
    println!("--- simplicity ---\n{simplicity_diff}");

    Ok(())
}
