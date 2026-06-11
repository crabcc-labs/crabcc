//! Lead Dev role: the plan gate plus the two things it does on APPROVE —
//! (a) pre-configure the coder team's model params (`TeamConfig`), and
//! (b) pre-inject crabcc context for the symbols the plan names.
//!
//! The gate decision is parsed from the Lead Dev agent's first line; the
//! `TeamConfig` is parsed from a fenced JSON block the same agent emits on
//! APPROVE. Parsing is lenient: a missing/garbled config falls back to
//! `TeamConfig::default()` so the run never stalls on the Lead Dev's
//! formatting.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

/// The Lead Dev's verdict on a plan.
pub enum Gate {
    /// Approved, with the team config the Lead Dev chose for the coders.
    Approve(Box<TeamConfig>),
    /// Needs another round; carries the actionable notes for the Planner.
    Revise(String),
    /// Hard stop on a project-rule violation; carries the reason.
    Stop(String),
}

/// Per-coder model params the Lead Dev pre-configures before fanout. Applied to
/// each coder `AgentBuilder` at build time. Fields map to rig 0.38 as follows:
/// `temperature`/`max_tokens`/`tool_choice` are first-class `AgentBuilder`
/// methods; `top_p`/`seed`/`reasoning_effort`/`max_reasoning_tokens` have no
/// typed setter, so they are flattened into the request body via
/// `.additional_params(json!({...}))` (the OpenAI provider's request struct
/// `#[serde(flatten)]`s that field).
#[derive(Debug, Clone, Deserialize)]
pub struct TeamConfig {
    pub temperature: f64,
    pub top_p: f64,
    pub seed: u64,
    pub max_tokens: u64,
    /// `"auto" | "none" | "required"` — maps to `rig::completion::message::ToolChoice`.
    pub tool_choice: String,
    /// OpenAI reasoning effort hint, e.g. `"low" | "medium" | "high"`.
    pub reasoning_effort: String,
    pub max_reasoning_tokens: u64,
}

impl Default for TeamConfig {
    /// Sensible coder defaults: low temperature for deterministic diffs, tools
    /// available (coders use the read tools), modest reasoning budget.
    fn default() -> Self {
        Self {
            temperature: 0.2,
            top_p: 0.95,
            seed: 7,
            max_tokens: 4096,
            tool_choice: "auto".to_string(),
            reasoning_effort: "low".to_string(),
            max_reasoning_tokens: 2048,
        }
    }
}

/// Parse the Lead Dev's free-text response into a `Gate`. Decision is the first
/// non-empty line; on APPROVE we additionally try to extract a fenced JSON
/// `TeamConfig`, falling back to the default if absent or unparseable.
pub fn parse_gate(response: &str) -> Gate {
    let first = response
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_uppercase();

    if first.starts_with("STOP") {
        Gate::Stop(response.trim().to_string())
    } else if first.starts_with("APPROVE") {
        let cfg = extract_team_config(response).unwrap_or_default();
        Gate::Approve(Box::new(cfg))
    } else {
        // Anything that is not STOP/APPROVE (including an explicit "REVISE") is
        // treated as a revision request; the whole response is the notes.
        Gate::Revise(response.trim().to_string())
    }
}

/// Pull a `TeamConfig` out of the first ```json fenced block in the response.
fn extract_team_config(response: &str) -> Option<TeamConfig> {
    let after_fence = response.split("```json").nth(1)?;
    let body = after_fence.split("```").next()?;
    serde_json::from_str(body.trim()).ok()
}

/// Run `crabcc lookup <args...>` with cwd = `repo`, returning stdout on success
/// or `None` on any failure (missing binary, non-zero exit). Pre-injection is
/// best-effort: a failed lookup just means the coders fetch it themselves.
fn crabcc_lookup(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("crabcc")
        .arg("lookup")
        .args(args)
        .current_dir(repo)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

/// Extract bare identifiers the plan names (CamelCase or snake_case, len >= 3)
/// to feed the crabcc lookups. Heuristic by design — the Lead Dev's plan is
/// prose, and over-fetching a few extra symbols is cheaper than a coder round
/// trip. Capped so a verbose plan can't trigger hundreds of lookups.
fn symbols_in_plan(plan: &str) -> Vec<String> {
    const MAX_SYMBOLS: usize = 12;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for raw in plan.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        let tok = raw.trim_matches('_');
        if tok.len() < 3 {
            continue;
        }
        let camel = tok.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && tok.chars().any(|c| c.is_ascii_lowercase());
        let snake = tok.contains('_') && tok.chars().all(|c| c.is_ascii_lowercase() || c == '_');
        if (camel || snake) && seen.insert(tok.to_string()) {
            out.push(tok.to_string());
            if out.len() >= MAX_SYMBOLS {
                break;
            }
        }
    }
    out
}

/// Pre-inject context: for each symbol the plan names, run crabcc `sym` and
/// `refs`; collect into a single block the coders receive alongside the plan.
/// Returns an empty string if crabcc is unavailable or no symbols matched.
pub fn preinject_context(repo: &Path, plan: &str) -> String {
    let symbols = symbols_in_plan(plan);
    if symbols.is_empty() {
        return String::new();
    }

    let mut buf = String::new();
    for sym in &symbols {
        if let Some(def) = crabcc_lookup(repo, &["sym", sym]) {
            if !def.trim().is_empty() {
                buf.push_str(&format!("## {sym} (definition)\n{}\n\n", def.trim()));
            }
        }
        if let Some(refs) = crabcc_lookup(repo, &["refs", sym]) {
            if !refs.trim().is_empty() {
                buf.push_str(&format!("## {sym} (references)\n{}\n\n", refs.trim()));
            }
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approve_without_config_uses_defaults() {
        match parse_gate("APPROVE — looks good") {
            Gate::Approve(cfg) => assert_eq!(cfg.temperature, TeamConfig::default().temperature),
            _ => panic!("expected APPROVE"),
        }
    }

    #[test]
    fn approve_parses_fenced_config() {
        let resp = "APPROVE\n\n```json\n{\
            \"temperature\":0.7,\"top_p\":0.9,\"seed\":42,\"max_tokens\":8192,\
            \"tool_choice\":\"required\",\"reasoning_effort\":\"high\",\
            \"max_reasoning_tokens\":4096}\n```";
        match parse_gate(resp) {
            Gate::Approve(cfg) => {
                assert_eq!(cfg.temperature, 0.7);
                assert_eq!(cfg.seed, 42);
                assert_eq!(cfg.tool_choice, "required");
                assert_eq!(cfg.reasoning_effort, "high");
            }
            _ => panic!("expected APPROVE"),
        }
    }

    #[test]
    fn stop_and_revise_are_distinguished() {
        assert!(matches!(parse_gate("STOP: violates rule X"), Gate::Stop(_)));
        assert!(matches!(parse_gate("REVISE\n1. do this"), Gate::Revise(_)));
        // A bare unrecognized response is treated as revise, not approve.
        assert!(matches!(parse_gate("hmm, not sure"), Gate::Revise(_)));
    }

    #[test]
    fn symbol_extraction_filters_and_caps() {
        let plan = "Modify CrabccSym and read_file; touch the io path. ok no.";
        let syms = symbols_in_plan(plan);
        assert!(syms.contains(&"CrabccSym".to_string()));
        assert!(syms.contains(&"read_file".to_string()));
        // "ok" and "no" are too short / not identifiers.
        assert!(!syms.iter().any(|s| s == "ok" || s == "no"));
    }
}
