//! The Phase-1 core graph: an explicit node state machine with round caps.
//!
//! Encoded LangGraph-style — each node is a step that reads/writes the shared
//! `GraphState`, emits exactly one `TraceEvent`, and the transitions (including
//! the two capped loops) are spelled out in `run`. Caps never hang: on
//! exhaustion the graph prints a warning and proceeds. A STOP from the Lead Dev
//! is the only early exit.

use anyhow::{Context, Result};
use rig::completion::Prompt;
use rig::providers::openai;

use crate::agents::{self, CoderLens};
use crate::lead::{self, Gate, TeamConfig};
use crate::trace::{Decision, Span};

/// Cap on Planner <-> Lead Dev review rounds before proceeding with a warning.
const MAX_LEAD_ROUNDS: usize = 3;
/// Cap on Synthesizer <-> Reviewer rounds before proceeding with a warning.
const MAX_REVIEW_ROUNDS: usize = 2;

/// Model ids resolved once from the environment, passed to each node.
pub struct Models {
    pub coder: String,
    pub lead: String,
    pub synth: String,
    pub review: String,
}

/// The graph's shared state: inputs the user gave plus everything the nodes
/// accrete as the run proceeds. Only `run` mutates it.
pub struct GraphState<'a> {
    client: &'a openai::CompletionsClient,
    models: &'a Models,
    repo: &'a std::path::Path,
    task: &'a str,

    plan: String,
    team_config: TeamConfig,
    injected_ctx: String,
    candidates: Vec<(String, String)>, // (lens name, diff)
    final_diff: String,
    review_notes: String,
}

/// The outcome the graph hands back to `main` after EMIT. The plan, commit
/// message, and candidate diffs are printed by the EMIT node itself; `main`
/// only needs the final diff (for `--apply`) and whether the Lead Dev stopped.
pub struct Outcome {
    pub final_diff: String,
    /// `true` if the Lead Dev hard-stopped on a rule violation (no diff).
    pub stopped: bool,
}

impl<'a> GraphState<'a> {
    pub fn new(
        client: &'a openai::CompletionsClient,
        models: &'a Models,
        repo: &'a std::path::Path,
        task: &'a str,
    ) -> Self {
        Self {
            client,
            models,
            repo,
            task,
            plan: String::new(),
            team_config: TeamConfig::default(),
            injected_ctx: String::new(),
            candidates: Vec::new(),
            final_diff: String::new(),
            review_notes: String::new(),
        }
    }

    /// Drive the full graph. Each `?` failure is a node error already traced.
    pub async fn run(mut self) -> Result<Outcome> {
        self.node_plan().await?;

        // LEAD GATE — loop REVISE up to the cap; STOP exits early.
        match self.node_lead_gate().await? {
            LeadResult::Approved => {}
            // The STOP reason was already printed by the gate node.
            LeadResult::Stopped(_reason) => {
                return Ok(Outcome {
                    final_diff: String::new(),
                    stopped: true,
                });
            }
            LeadResult::CapReached => {
                eprintln!(
                    "[warning] plan not approved after {MAX_LEAD_ROUNDS} rounds; proceeding \
                     with the last plan"
                );
            }
        }

        self.node_fanout().await?;

        // SYNTH + REVIEW — loop REQUEST-CHANGES back to SYNTH up to the cap.
        self.node_synth().await?;
        for round in 1..=MAX_REVIEW_ROUNDS {
            match self.node_review(round).await? {
                ReviewResult::Approved => break,
                ReviewResult::ChangesRequested => {
                    if round == MAX_REVIEW_ROUNDS {
                        eprintln!(
                            "[warning] reviewer still requesting changes after \
                             {MAX_REVIEW_ROUNDS} rounds; proceeding with the last diff"
                        );
                        break;
                    }
                    // Re-synthesize with the review notes folded in.
                    self.node_synth().await?;
                }
            }
        }

        self.node_self_review().await?;
        let outcome = self.node_emit();
        Ok(outcome)
    }

    // ---- 1. PLAN ------------------------------------------------------
    async fn node_plan(&mut self) -> Result<()> {
        let span = Span::start("plan", "planner", &self.models.lead);
        let planner = agents::planner(self.client, &self.models.lead, self.repo);
        let res = planner
            .prompt(format!(
                "Task:\n{}\n\nDraft an implementation plan for this task in the repo.",
                self.task
            ))
            .max_turns(agents::max_turns())
            .await;
        match res {
            Ok(plan) => {
                self.plan = plan;
                span.finish(Decision::Produced, None);
                Ok(())
            }
            Err(e) => {
                span.finish(Decision::Error, Some(e.to_string()));
                Err(e).context("planner failed")
            }
        }
    }

    // ---- 2. LEAD GATE -------------------------------------------------
    async fn node_lead_gate(&mut self) -> Result<LeadResult> {
        let lead = agents::lead_dev(self.client, &self.models.lead);

        for round in 1..=MAX_LEAD_ROUNDS {
            let span = Span::start("lead_gate", "lead-dev", &self.models.lead).round(round);
            let res = lead
                .prompt(format!(
                    "Task:\n{}\n\nProposed plan:\n{}\n\nReview it.",
                    self.task, self.plan
                ))
                .await;
            let response = match res {
                Ok(r) => r,
                Err(e) => {
                    span.finish(Decision::Error, Some(e.to_string()));
                    return Err(e).context("lead dev review failed");
                }
            };

            match lead::parse_gate(&response) {
                Gate::Approve(cfg) => {
                    self.team_config = *cfg;
                    span.finish(Decision::Approve, None);
                    // On APPROVE the Lead Dev also pre-injects crabcc context for
                    // the symbols the plan names, so the coders start with the
                    // relevant code in hand. Best-effort: empty if crabcc absent.
                    self.injected_ctx = lead::preinject_context(self.repo, &self.plan);
                    return Ok(LeadResult::Approved);
                }
                Gate::Stop(reason) => {
                    eprintln!("[lead-dev] STOP: {reason}");
                    span.finish(Decision::Stop, Some(reason.clone()));
                    return Ok(LeadResult::Stopped(reason));
                }
                Gate::Revise(notes) => {
                    if round == MAX_LEAD_ROUNDS {
                        // Cap hit: trace it as such (not another Revise) and
                        // proceed rather than loop forever.
                        span.finish(Decision::CapReached, Some(notes));
                        return Ok(LeadResult::CapReached);
                    }
                    span.finish(Decision::Revise, Some(notes.clone()));
                    // Feed notes back to the Planner for a revision.
                    self.revise_plan(&notes).await?;
                }
            }
        }
        Ok(LeadResult::CapReached)
    }

    async fn revise_plan(&mut self, notes: &str) -> Result<()> {
        let span = Span::start("plan", "planner", &self.models.lead);
        let planner = agents::planner(self.client, &self.models.lead, self.repo);
        let res = planner
            .prompt(format!(
                "Task:\n{}\n\nYour previous plan:\n{}\n\nThe Lead Dev asked for these \
                 revisions:\n{}\n\nReturn the revised plan.",
                self.task, self.plan, notes
            ))
            .max_turns(agents::max_turns())
            .await;
        match res {
            Ok(plan) => {
                self.plan = plan;
                span.finish(Decision::Produced, None);
                Ok(())
            }
            Err(e) => {
                span.finish(Decision::Error, Some(e.to_string()));
                Err(e).context("planner revision failed")
            }
        }
    }

    // ---- 3. FANOUT (3 coders in parallel) -----------------------------
    async fn node_fanout(&mut self) -> Result<()> {
        let coder_prompt = format!(
            "Task:\n{}\n\nApproved plan:\n{}\n\n{}\nImplement it. Output ONLY a unified diff.",
            self.task,
            self.plan,
            if self.injected_ctx.is_empty() {
                String::new()
            } else {
                format!("Pre-injected repo context:\n{}\n", self.injected_ctx)
            },
        );

        let safety = agents::coder(
            self.client,
            &self.models.coder,
            self.repo,
            &CoderLens::Safety,
            &self.team_config,
        );
        let perf = agents::coder(
            self.client,
            &self.models.coder,
            self.repo,
            &CoderLens::Perf,
            &self.team_config,
        );
        let simplicity = agents::coder(
            self.client,
            &self.models.coder,
            self.repo,
            &CoderLens::Simplicity,
            &self.team_config,
        );

        // One span per coder; they run concurrently via tokio::join!.
        let s_safety = Span::start("fanout.safety", "coder-safety", &self.models.coder);
        let s_perf = Span::start("fanout.perf", "coder-perf", &self.models.coder);
        let s_simp = Span::start("fanout.simplicity", "coder-simplicity", &self.models.coder);

        let (r_safety, r_perf, r_simp) = tokio::join!(
            safety.prompt(coder_prompt.clone()).max_turns(agents::max_turns()),
            perf.prompt(coder_prompt.clone()).max_turns(agents::max_turns()),
            simplicity.prompt(coder_prompt.clone()).max_turns(agents::max_turns()),
        );

        let safety_diff = finish_coder(s_safety, r_safety, "safety coder")?;
        let perf_diff = finish_coder(s_perf, r_perf, "perf coder")?;
        let simp_diff = finish_coder(s_simp, r_simp, "simplicity coder")?;

        self.candidates = vec![
            ("safety".to_string(), safety_diff),
            ("perf".to_string(), perf_diff),
            ("simplicity".to_string(), simp_diff),
        ];
        Ok(())
    }

    // ---- 4. SYNTH -----------------------------------------------------
    async fn node_synth(&mut self) -> Result<()> {
        let span = Span::start("synth", "synthesizer", &self.models.synth);
        let synth = agents::synthesizer(self.client, &self.models.synth);

        // Fold prior review notes in on re-synthesis rounds.
        let notes_block = if self.review_notes.is_empty() {
            String::new()
        } else {
            format!(
                "\nThe reviewer requested these changes on the previous diff; \
                 address them:\n{}\n",
                self.review_notes
            )
        };

        let mut prompt = format!("Task:\n{}\n\nApproved plan:\n{}\n", self.task, self.plan);
        for (lens, diff) in &self.candidates {
            prompt.push_str(&format!("\nCandidate diff ({lens} lens):\n{diff}\n"));
        }
        prompt.push_str(&notes_block);
        prompt.push_str("\nReconcile into ONE best-of-three unified diff.");

        let res = synth.prompt(prompt).await;
        match res {
            Ok(diff) => {
                self.final_diff = diff;
                span.finish(Decision::Produced, None);
                Ok(())
            }
            Err(e) => {
                span.finish(Decision::Error, Some(e.to_string()));
                Err(e).context("synthesizer failed")
            }
        }
    }

    // ---- 5. REVIEW ----------------------------------------------------
    async fn node_review(&mut self, round: usize) -> Result<ReviewResult> {
        let span = Span::start("review", "reviewer", &self.models.review).round(round);
        let reviewer = agents::reviewer(self.client, &self.models.review);
        let res = reviewer
            .prompt(format!(
                "Approved plan:\n{}\n\nFinal unified diff:\n{}\n\nReview it.",
                self.plan, self.final_diff
            ))
            .await;
        let response = match res {
            Ok(r) => r,
            Err(e) => {
                span.finish(Decision::Error, Some(e.to_string()));
                return Err(e).context("reviewer failed");
            }
        };

        let approved = response
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_uppercase()
            .starts_with("APPROVE");

        if approved {
            self.review_notes.clear();
            span.finish(Decision::Approve, None);
            Ok(ReviewResult::Approved)
        } else {
            self.review_notes = response.trim().to_string();
            // On the final round a changes-request is a cap, not another loop;
            // trace it so Phase 2 can distinguish "proceeded despite notes".
            let decision = if round == MAX_REVIEW_ROUNDS {
                Decision::CapReached
            } else {
                Decision::Revise
            };
            span.finish(decision, Some(self.review_notes.clone()));
            Ok(ReviewResult::ChangesRequested)
        }
    }

    // ---- 6. SELF-REVIEW ----------------------------------------------
    async fn node_self_review(&mut self) -> Result<()> {
        let span = Span::start("self_review", "synthesizer", &self.models.synth);
        let reviewer = agents::self_reviewer(self.client, &self.models.synth);
        let notes = if self.review_notes.is_empty() {
            "(reviewer approved; no outstanding notes)".to_string()
        } else {
            self.review_notes.clone()
        };
        let res = reviewer
            .prompt(format!(
                "Approved plan:\n{}\n\nYour final diff:\n{}\n\nReviewer notes:\n{}\n\n\
                 Self-review and output the final unified diff.",
                self.plan, self.final_diff, notes
            ))
            .await;
        match res {
            Ok(diff) => {
                self.final_diff = diff;
                span.finish(Decision::Produced, None);
                Ok(())
            }
            Err(e) => {
                span.finish(Decision::Error, Some(e.to_string()));
                Err(e).context("self-review failed")
            }
        }
    }

    // ---- 7. EMIT ------------------------------------------------------
    fn node_emit(self) -> Outcome {
        let span = Span::start("emit", "graph", "-");
        let commit_message = commit_message_for(self.task, &self.plan);

        println!("===== APPROVED PLAN =====\n{}\n", self.plan);
        println!("===== FINAL UNIFIED DIFF =====\n{}\n", self.final_diff);
        println!("===== COMMIT MESSAGE =====\n{commit_message}\n");
        println!(
            "===== PING LEAD =====\nTask handled. Plan approved, diff synthesized from 3 \
             lenses, reviewed, self-reviewed. Diff is NOT applied; re-run with --apply to \
             `git apply` it.\n"
        );
        println!("===== CANDIDATE DIFFS (for transparency) =====");
        for (lens, diff) in &self.candidates {
            println!("--- {lens} ---\n{diff}\n");
        }

        span.finish(Decision::Produced, None);
        Outcome {
            final_diff: self.final_diff,
            stopped: false,
        }
    }
}

/// First line of the task becomes the commit subject; the rest is a short body.
fn commit_message_for(task: &str, _plan: &str) -> String {
    let subject = task.lines().next().unwrap_or(task).trim();
    // Keep the subject within a conventional 72-col budget.
    let subject = if subject.len() > 72 {
        &subject[..72]
    } else {
        subject
    };
    format!("{subject}\n\nImplemented via swe-team (plan -> 3-lens fanout -> synth -> review).")
}

/// Resolve a coder's join result into its diff, finishing its trace span.
fn finish_coder(
    span: Span,
    res: Result<String, rig::completion::PromptError>,
    label: &str,
) -> Result<String> {
    match res {
        Ok(diff) => {
            span.finish(Decision::Produced, None);
            Ok(diff)
        }
        Err(e) => {
            span.finish(Decision::Error, Some(e.to_string()));
            Err(e).with_context(|| format!("{label} failed"))
        }
    }
}

enum LeadResult {
    Approved,
    Stopped(String),
    CapReached,
}

enum ReviewResult {
    Approved,
    ChangesRequested,
}
