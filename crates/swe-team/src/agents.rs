//! Agent preambles + builders for the SWE-team pipeline.
//!
//! All agents are built from one Chat-Completions client (pointed at the
//! gateway); only the model id, preamble, and sampling params differ. The
//! Planner and the 3 Coders get the crabcc-backed read tools; the Lead Dev,
//! Synthesizer, and Reviewer reason over text the graph hands them.

use std::path::Path;

use rig::agent::{Agent, AgentBuilder};
use rig::client::CompletionClient;
use rig::completion::message::ToolChoice;
use rig::providers::openai;
use serde_json::json;

use crate::lead::TeamConfig;
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

/// The Lead Dev gates the plan AND pre-configures the coder team. It must emit
/// one of STOP / REVISE / APPROVE on the first line; on APPROVE it also returns
/// a fenced JSON `TeamConfig` the graph parses (see `lead::TeamConfig`).
const LEAD_DEV_PREAMBLE: &str = "\
You are the Lead Developer. You gate an implementation plan before any code is \
written, and you pre-configure the coder team. Judge whether the plan is \
correct, complete, and low-risk. Respond on the FIRST line with exactly one of:\n\
  STOP    — the plan violates a hard project rule; add the reason after it.\n\
  REVISE  — fixable problems; list the specific, numbered changes the Planner must make.\n\
  APPROVE — the plan is sound.\n\
Only REVISE for substantive problems, not style. On APPROVE, after a one-line \
rationale, emit a fenced JSON block (```json ... ```) with the model params for \
the coders, choosing values appropriate to the task's risk:\n\
  {\"temperature\":0.2,\"top_p\":0.95,\"seed\":7,\"max_tokens\":4096,\
\"tool_choice\":\"auto\",\"reasoning_effort\":\"low\",\"max_reasoning_tokens\":2048}\n\
If you omit or malform the block, safe defaults are used.";

/// The three engineering lenses. Each coder gets the same approved plan +
/// pre-injected context but a different bias, then emits one candidate diff.
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

/// The reviewer persona: deliberately relaxed on style/nits, but paranoid on
/// the two axes that bite users — security and UI.
const REVIEWER_PREAMBLE: &str = "\
You are the Reviewer. You are chill and NOT strict: do not nitpick style, \
naming, or micro-optimizations, and do not block on taste. You make exactly TWO \
exceptions where you are paranoid and thorough: SECURITY (injection, auth, \
unsafe input handling, secrets, path traversal, unsafe code) and UI/UX \
(anything user-facing: output, prompts, error messages, accessibility). Review \
the final unified diff against the plan. Respond on the FIRST line with exactly \
one of:\n\
  APPROVE\n\
  REQUEST-CHANGES\n\
If REQUEST-CHANGES, list the specific security/UI concerns (numbered). Anything \
outside security/UI: let it pass.";

/// The synthesizer's self-review pass over its own final diff.
const SELF_REVIEW_PREAMBLE: &str = "\
You are the Synthesizer doing a final self-review. Given the approved plan, your \
final unified diff, and the reviewer's notes, check the diff actually \
implements the plan and addresses the notes. If it is good, output the diff \
unchanged. If you spot a real gap, output a corrected unified diff. Output ONLY \
a unified diff (git-style) — no prose.";

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

/// Map the Lead Dev's `tool_choice` string to rig's `ToolChoice`. Unknown
/// values fall back to `Auto` rather than erroring — the config is advisory.
fn tool_choice_from(s: &str) -> ToolChoice {
    match s.to_ascii_lowercase().as_str() {
        "none" => ToolChoice::None,
        "required" => ToolChoice::Required,
        _ => ToolChoice::Auto,
    }
}

/// Apply a `TeamConfig` to a coder builder. `temperature`/`max_tokens`/
/// `tool_choice` use first-class rig setters; `top_p`/`seed`/`reasoning_effort`/
/// `max_reasoning_tokens` have no typed setter in rig 0.38, so they ride in via
/// `additional_params`, which the OpenAI provider flattens into the request
/// body.
fn apply_team_config(
    builder: AgentBuilder<openai::CompletionModel>,
    cfg: &TeamConfig,
) -> AgentBuilder<openai::CompletionModel> {
    builder
        .temperature(cfg.temperature)
        .max_tokens(cfg.max_tokens)
        .tool_choice(tool_choice_from(&cfg.tool_choice))
        .additional_params(json!({
            "top_p": cfg.top_p,
            "seed": cfg.seed,
            "reasoning_effort": cfg.reasoning_effort,
            "max_reasoning_tokens": cfg.max_reasoning_tokens,
        }))
}

/// Attach the four read-only repo tools (all bound to `repo`) and build.
fn with_repo_tools(builder: AgentBuilder<openai::CompletionModel>, repo: &Path) -> SweAgent {
    builder
        .tool(CrabccSym {
            repo: repo.to_path_buf(),
        })
        .tool(CrabccRefs {
            repo: repo.to_path_buf(),
        })
        .tool(CrabccOutline {
            repo: repo.to_path_buf(),
        })
        .tool(ReadFile {
            repo: repo.to_path_buf(),
        })
        .build()
}

pub fn planner(client: &openai::CompletionsClient, model: &str, repo: &Path) -> SweAgent {
    with_repo_tools(
        client
            .agent(model)
            .name("planner")
            .preamble(PLANNER_PREAMBLE),
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

/// Build a coder for `lens`, with the Lead Dev's pre-configured `TeamConfig`
/// applied to its sampling/reasoning params.
pub fn coder(
    client: &openai::CompletionsClient,
    model: &str,
    repo: &Path,
    lens: &CoderLens,
    cfg: &TeamConfig,
) -> SweAgent {
    let preamble = format!("{CODER_SHARED}\n\n{}", lens.lens_text());
    let builder = client
        .agent(model)
        .name(&format!("coder-{}", lens.name()))
        .preamble(&preamble);
    with_repo_tools(apply_team_config(builder, cfg), repo)
}

pub fn synthesizer(client: &openai::CompletionsClient, model: &str) -> SweAgent {
    client
        .agent(model)
        .name("synthesizer")
        .preamble(SYNTH_PREAMBLE)
        .build()
}

pub fn reviewer(client: &openai::CompletionsClient, model: &str) -> SweAgent {
    client
        .agent(model)
        .name("reviewer")
        .preamble(REVIEWER_PREAMBLE)
        .build()
}

pub fn self_reviewer(client: &openai::CompletionsClient, model: &str) -> SweAgent {
    client
        .agent(model)
        .name("self-reviewer")
        .preamble(SELF_REVIEW_PREAMBLE)
        .build()
}
