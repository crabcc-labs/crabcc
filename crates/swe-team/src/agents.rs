//! Agent preambles + builders for the SWE-team pipeline.
//!
//! All agents are built from one Chat-Completions client (pointed at the
//! gateway); only the model id and preamble differ. The Planner and the 3
//! Coders get the crabcc-backed read tools; the Lead Dev and Synthesizer
//! reason over text the pipeline hands them and need no tools.

use std::path::Path;

use rig::agent::Agent;
use rig::client::CompletionClient;
use rig::providers::openai;

use crate::tools::{CrabccOutline, CrabccRefs, CrabccSym, ReadFile};

/// Concrete agent type produced by this module: an agent over the OpenAI
/// Chat-Completions model with the default (no-op) prompt hook.
pub type SweAgent = Agent<openai::CompletionModel>;

/// Tool-using agents may need several turns (think -> call tool -> think ->
/// answer); cap the loop so a misbehaving model can't spin forever.
const TOOL_MAX_TURNS: usize = 8;
pub const MAX_TURNS: usize = TOOL_MAX_TURNS;

const PLANNER_PREAMBLE: &str = "\
You are the Planner on a software-engineering team. Given a task and a target \
repository, draft a concrete implementation plan. Use the read-only repo tools \
(crabcc_sym, crabcc_refs, crabcc_outline, read_file) to ground the plan in the \
actual code before proposing changes. Output a plan that names the exact files \
to create or modify, the approach, and an ordered list of steps. Do not write \
the code or a diff — only the plan. Be specific and concise.";

const LEAD_DEV_PREAMBLE: &str = "\
You are the Lead Developer reviewing an implementation plan before any code is \
written. Judge whether the plan is correct, complete, and low-risk for the \
stated task. Respond on the FIRST line with exactly one of:\n\
  APPROVE\n\
  REVISE\n\
If APPROVE, optionally add a one-line rationale. If REVISE, list the specific, \
actionable changes the Planner must make (numbered). Be strict but fair; only \
REVISE for substantive problems, not style.";

/// The three engineering lenses. Each coder gets the same approved plan but a
/// different bias, then emits one candidate unified diff.
const CODER_SHARED: &str = "\
You are a Coder implementing an already-approved plan. Use the read-only repo \
tools to inspect exact code before editing. Output ONLY a single unified diff \
(git-style, with ---/+++ headers and @@ hunks) that implements the plan. No \
prose before or after the diff.";

const SAFETY_LENS: &str = "Your engineering lens is SAFETY: prioritize \
correctness, edge cases, and error handling. Never introduce `unsafe`. Validate \
inputs and handle failure paths explicitly.";

const PERF_LENS: &str = "Your engineering lens is PERFORMANCE: minimize \
allocations, keep the hot path tight, and prefer zero-copy where it does not \
hurt clarity.";

const SIMPLICITY_LENS: &str = "Your engineering lens is SIMPLICITY: write the \
minimal, idiomatic, DRY solution. No speculative abstraction or configurability \
that the task did not ask for.";

const SYNTH_PREAMBLE: &str = "\
You are the Synthesizer. You are given the task, the approved plan, and three \
candidate unified diffs written by coders with different lenses (safety, \
performance, simplicity). Reconcile them into ONE best-of-three unified diff: \
take the safest correct structure, fold in worthwhile performance and \
simplicity wins, and resolve conflicts in favor of correctness. Output ONLY the \
final unified diff (git-style) — no prose.";

pub enum CoderLens {
    Safety,
    Perf,
    Simplicity,
}

impl CoderLens {
    pub fn name(&self) -> &'static str {
        match self {
            CoderLens::Safety => "safety",
            CoderLens::Perf => "perf",
            CoderLens::Simplicity => "simplicity",
        }
    }

    fn lens_text(&self) -> &'static str {
        match self {
            CoderLens::Safety => SAFETY_LENS,
            CoderLens::Perf => PERF_LENS,
            CoderLens::Simplicity => SIMPLICITY_LENS,
        }
    }
}

/// Attach the four read-only repo tools (all bound to `repo`) and build.
fn with_repo_tools(builder: rig::agent::AgentBuilder<openai::CompletionModel>, repo: &Path) -> SweAgent {
    builder
        .tool(CrabccSym { repo: repo.to_path_buf() })
        .tool(CrabccRefs { repo: repo.to_path_buf() })
        .tool(CrabccOutline { repo: repo.to_path_buf() })
        .tool(ReadFile { repo: repo.to_path_buf() })
        .build()
}

pub fn planner(client: &openai::CompletionsClient, model: &str, repo: &Path) -> SweAgent {
    with_repo_tools(
        client.agent(model).name("planner").preamble(PLANNER_PREAMBLE),
        repo,
    )
}

pub fn lead_dev(client: &openai::CompletionsClient, model: &str) -> SweAgent {
    client
        .agent(model)
        .name("lead-dev")
        .preamble(LEAD_DEV_PREAMBLE)
        .build()
}

pub fn coder(
    client: &openai::CompletionsClient,
    model: &str,
    repo: &Path,
    lens: &CoderLens,
) -> SweAgent {
    let preamble = format!("{CODER_SHARED}\n\n{}", lens.lens_text());
    with_repo_tools(
        client
            .agent(model)
            .name(&format!("coder-{}", lens.name()))
            .preamble(&preamble),
        repo,
    )
}

pub fn synthesizer(client: &openai::CompletionsClient, model: &str) -> SweAgent {
    client
        .agent(model)
        .name("synthesizer")
        .preamble(SYNTH_PREAMBLE)
        .build()
}
