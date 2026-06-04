//! `crabcc research` — bridge `crabcc init`'s research-plan handoff to the
//! deep-research skill, and route the findings back into the `research`
//! memory wing so `crabcc enrich` surfaces them.
//!
//! The traceable loop:
//!   1. `crabcc init` writes `.crabcc/onboard/research-plan.json` — one task
//!      per dependency (topic, ecosystem, doc_url, queries) plus the
//!      `result_wing` findings should be stored in.
//!   2. `crabcc research brief` reads that plan and emits a ready-to-run
//!      deep-research brief: what to investigate per topic AND the storage
//!      contract (`crabcc research ingest --topic <t>`). Hand it to the
//!      deep-research skill.
//!   3. The skill researches, then pipes each topic's cited report through
//!      `crabcc research ingest --topic <t>` → stored in the `research` wing,
//!      room = topic, source = `research:<topic>`.
//!   4. `crabcc enrich "<topic>"` pulls it back as bounded prompt context.
//!   5. `crabcc research status` shows which plan topics have findings yet —
//!      plan in → findings in a known wing → enrich out, end to end.
//!
//! Reads only the memory Palace + the onboard plan file; no symbol Store.

use anyhow::{Context, Result};
use clap::Subcommand;
use crabcc_memory::Palace;
use std::path::{Path, PathBuf};

/// Wing findings land in when the plan file is absent or omits one. Kept in
/// sync with `init_cmd::RESULT_WING`.
const DEFAULT_WING: &str = "research";

#[derive(Subcommand, Debug)]
pub enum ResearchCmd {
    /// Emit a deep-research brief from `.crabcc/onboard/research-plan.json`:
    /// per-topic queries + the storage contract the skill should follow.
    Brief {
        /// Emit the raw research-plan.json instead of the prose brief.
        #[arg(long)]
        json: bool,
    },
    /// Store one topic's findings into the research wing (room = topic), so
    /// `crabcc enrich "<topic>"` surfaces it. Body from `--file`, or stdin.
    Ingest {
        /// Topic this finding is about (becomes the drawer `room`).
        #[arg(long)]
        topic: String,
        /// Read the finding body from this file. Omit (or `-`) to read stdin.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Wing to store under. Defaults to the plan's `result_wing`
        /// (falling back to `research`).
        #[arg(long)]
        wing: Option<String>,
    },
    /// Show, per plan topic, whether the research wing has findings yet.
    Status {
        /// Emit a JSON report instead of the human checklist.
        #[arg(long)]
        json: bool,
    },
}

/// One task in the machine-readable plan. Mirrors `init_cmd::ResearchTask`'s
/// shape (deserialize side); unknown fields are ignored and missing optional
/// fields default so an older/newer plan still parses.
#[derive(serde::Deserialize)]
struct PlanTask {
    topic: String,
    #[serde(default)]
    ecosystem: String,
    #[serde(default)]
    doc_url: String,
    #[serde(default)]
    queries: Vec<String>,
}

/// The plan `crabcc init` writes. `result_wing` defaults so a hand-written or
/// truncated plan still resolves a wing.
#[derive(serde::Deserialize)]
struct Plan {
    #[serde(default = "default_wing")]
    result_wing: String,
    #[serde(default)]
    tasks: Vec<PlanTask>,
}

fn default_wing() -> String {
    DEFAULT_WING.to_string()
}

pub fn run(root: &Path, cmd: ResearchCmd) -> Result<()> {
    match cmd {
        ResearchCmd::Brief { json } => brief(root, json),
        ResearchCmd::Ingest { topic, file, wing } => {
            ingest(root, &topic, file.as_deref(), wing.as_deref())
        }
        ResearchCmd::Status { json } => status(root, json),
    }
}

/// Locate `.crabcc/onboard/research-plan.json`, walking up from `start` so
/// `crabcc research` works from a subdirectory of the onboarded repo.
fn find_plan(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let p = d.join(".crabcc").join("onboard").join("research-plan.json");
        if p.is_file() {
            return Some(p);
        }
        dir = d.parent();
    }
    None
}

fn load_plan(root: &Path) -> Result<Plan> {
    let path = find_plan(root).context(
        "no research-plan.json found — run `crabcc init` first to generate the onboarding plan",
    )?;
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn brief(root: &Path, json: bool) -> Result<()> {
    if json {
        // Byte-identical passthrough of the plan file — no re-serialization
        // drift for the programmatic consumer.
        let path = find_plan(root).context(
            "no research-plan.json found — run `crabcc init` first to generate the onboarding plan",
        )?;
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        print!("{raw}");
        return Ok(());
    }
    let plan = load_plan(root)?;
    print!("{}", render_brief(&plan));
    Ok(())
}

/// Render the plan as a deep-research brief: a short preamble with the storage
/// contract, then one numbered section per topic with its docs + queries.
fn render_brief(plan: &Plan) -> String {
    let mut s = String::new();
    let n = plan.tasks.len();
    s.push_str("# Deep-research brief (crabcc init handoff)\n\n");
    if n == 0 {
        s.push_str(
            "The research plan has no topics. Run `crabcc init` in a repo with \
             dependency manifests to populate it.\n",
        );
        return s;
    }
    s.push_str(&format!(
        "Research the following {n} topic(s) for this codebase. For each: run \
         focused web searches, fetch and adversarially verify sources, then \
         synthesize a short cited summary.\n\n",
    ));
    s.push_str("Store each topic's findings so the rest of crabcc can use them:\n\n");
    s.push_str(&format!(
        "    <report> | crabcc research ingest --topic \"<topic>\"\n\n\
         Findings land in the `{}` memory wing (room = topic); retrieve later \
         with `crabcc enrich \"<topic>\"`. Track progress with `crabcc research \
         status`.\n\n",
        plan.result_wing,
    ));
    for (i, t) in plan.tasks.iter().enumerate() {
        let eco = if t.ecosystem.is_empty() {
            String::new()
        } else {
            format!("  ({})", t.ecosystem)
        };
        s.push_str(&format!("## {}. {}{eco}\n", i + 1, t.topic));
        if !t.doc_url.is_empty() {
            s.push_str(&format!("Docs: {}\n", t.doc_url));
        }
        if !t.queries.is_empty() {
            s.push_str("Queries:\n");
            for q in &t.queries {
                s.push_str(&format!("- {q}\n"));
            }
        }
        s.push('\n');
    }
    s
}

fn ingest(root: &Path, topic: &str, file: Option<&Path>, wing: Option<&str>) -> Result<()> {
    if topic.trim().is_empty() {
        anyhow::bail!("--topic must not be empty");
    }
    let body = match file {
        Some(p) if p != Path::new("-") => {
            std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?
        }
        _ => {
            use std::io::Read as _;
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("read finding from stdin")?;
            s
        }
    };
    if body.trim().is_empty() {
        anyhow::bail!("empty finding body — pass `--file PATH` or pipe the report on stdin");
    }
    // Wing precedence: explicit `--wing` > the plan's result_wing > default.
    let wing = wing
        .map(String::from)
        .or_else(|| load_plan(root).ok().map(|p| p.result_wing))
        .unwrap_or_else(default_wing);

    let palace = Palace::open(root)?;
    let source = format!("research:{topic}");
    let session = std::env::var("TERM_SESSION_ID").ok();
    let id = palace.remember_in_session(&wing, Some(topic), &source, &body, session.as_deref())?;
    println!(
        "{}",
        serde_json::json!({"id": id, "wing": wing, "room": topic, "source": source})
    );
    Ok(())
}

fn status(root: &Path, json: bool) -> Result<()> {
    let plan = load_plan(root)?;
    let palace = Palace::open(root)?;
    // Rooms in the result wing that already hold at least one finding.
    let drawers = palace.list_drawers(Some(&plan.result_wing), 10_000)?;
    let done: std::collections::HashSet<&str> =
        drawers.iter().filter_map(|d| d.room.as_deref()).collect();

    let rows: Vec<(&str, bool)> = plan
        .tasks
        .iter()
        .map(|t| (t.topic.as_str(), done.contains(t.topic.as_str())))
        .collect();
    let done_n = rows.iter().filter(|(_, h)| *h).count();

    if json {
        let topics: Vec<_> = rows
            .iter()
            .map(|(t, h)| serde_json::json!({"topic": t, "done": h}))
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "wing": plan.result_wing,
                "total": rows.len(),
                "done": done_n,
                "topics": topics,
            })
        );
        return Ok(());
    }

    println!(
        "research status — {done_n}/{} topics have findings in wing `{}`:",
        rows.len(),
        plan.result_wing
    );
    for (t, h) in rows {
        println!("  [{}] {t}", if h { 'x' } else { ' ' });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(json: &str) -> Plan {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn brief_lists_topics_and_storage_contract() {
        let p = plan(
            r#"{"result_wing":"research","tasks":[
                {"topic":"tokio","ecosystem":"rust","doc_url":"https://docs.rs/tokio",
                 "queries":["tokio latest release","tokio pitfalls"]}
            ]}"#,
        );
        let b = render_brief(&p);
        assert!(b.contains("## 1. tokio  (rust)"));
        assert!(b.contains("Docs: https://docs.rs/tokio"));
        assert!(b.contains("- tokio latest release"));
        // The storage contract must name the ingest command + the wing.
        assert!(b.contains("crabcc research ingest --topic"));
        assert!(b.contains("`research` memory wing"));
        assert!(b.contains("crabcc enrich"));
    }

    #[test]
    fn brief_handles_empty_plan() {
        let p = plan(r#"{"result_wing":"research","tasks":[]}"#);
        let b = render_brief(&p);
        assert!(b.contains("no topics"));
        assert!(!b.contains("## 1."));
    }

    #[test]
    fn plan_defaults_missing_wing_and_optional_fields() {
        // Minimal plan: only topic present. Wing defaults; optional fields empty.
        let p = plan(r#"{"tasks":[{"topic":"serde"}]}"#);
        assert_eq!(p.result_wing, "research");
        assert_eq!(p.tasks.len(), 1);
        assert_eq!(p.tasks[0].topic, "serde");
        assert!(p.tasks[0].queries.is_empty());
        // A topic with no docs/queries renders cleanly (no "Docs:"/"Queries:").
        let b = render_brief(&p);
        assert!(b.contains("## 1. serde"));
        assert!(!b.contains("Docs:"));
        assert!(!b.contains("Queries:"));
    }

    #[test]
    fn find_plan_walks_up_from_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let onboard = tmp.path().join(".crabcc").join("onboard");
        std::fs::create_dir_all(&onboard).unwrap();
        std::fs::write(
            onboard.join("research-plan.json"),
            r#"{"result_wing":"research","tasks":[]}"#,
        )
        .unwrap();
        let sub = tmp.path().join("crates").join("deep").join("src");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(find_plan(&sub).is_some());
        // No plan above an unrelated dir.
        let other = tempfile::tempdir().unwrap();
        assert!(find_plan(other.path()).is_none());
    }
}
