//! `crabcc init` — onboard onto a fresh codebase.
//!
//! Kicks off background research so an agent walks into proper context:
//!
//! 1. **Detect** the stack from the repo's dependency manifests.
//! 2. **Crawl** each dependency's docs (docs.rs etc.) into the memory
//!    Palace in the *background* — deterministic, binary-only.
//! 3. **Plan** the deeper, search-driven research (latest blog posts /
//!    changelogs) for the agent to run with its deep-research skill, since
//!    the binary has no web search.
//! 4. **Overview + inject**: write a codebase overview and a SessionStart
//!    hook snippet so the first few prompts get the onboarding context
//!    plus `crabcc enrich` pointers.
//!
//! v1 is the focused slice: detection + background doc-crawl + the plan /
//! overview / hook artifacts under `.crabcc/onboard/`. The pure pieces
//! (manifest parsing, doc-URL derivation, artifact rendering) are unit
//! tested; the spawn + file writes are the integration glue.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

/// How many dependency doc-sites to crawl in the background. Kept modest so
/// onboarding is "medium-sized", not a full mirror.
const MAX_CRAWL_TOPICS: usize = 12;
/// Per-topic crawl budget for the background onboarding crawls.
const ONBOARD_DEPTH: &str = "1";
const ONBOARD_MAX_PAGES: &str = "15";

pub fn run(root: &Path) -> Result<()> {
    let topics = detect_topics(root);
    if topics.is_empty() {
        eprintln!(
            "init: no dependency manifests found under {} — nothing to research yet",
            root.display()
        );
    }

    let onboard_dir = root.join(".crabcc").join("onboard");
    std::fs::create_dir_all(&onboard_dir).context("create .crabcc/onboard")?;

    // Artifacts: research plan (for the agent's deep-research skill) +
    // codebase overview (for the SessionStart injection).
    std::fs::write(
        onboard_dir.join("research-plan.md"),
        render_research_plan(&topics),
    )?;
    std::fs::write(
        onboard_dir.join("overview.md"),
        render_overview(root, &topics),
    )?;

    // Background doc-crawl: spawn a detached `crabcc crawl … --remember` per
    // top dependency so the Palace fills while the agent gets going.
    let crawl_topics: Vec<&Topic> = topics.iter().take(MAX_CRAWL_TOPICS).collect();
    let spawned = spawn_background_crawls(&crawl_topics);

    eprintln!(
        "init: detected {} topic(s); crawling {} doc-site(s) in the background → memory wing=onboard",
        topics.len(),
        spawned,
    );
    eprintln!(
        "init: wrote {}/research-plan.md and overview.md",
        onboard_dir.display()
    );
    eprintln!("\n{}", hook_snippet(root));
    Ok(())
}

/// A thing worth researching — a dependency / library name plus the
/// ecosystem it came from (so we can derive the right doc URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topic {
    pub name: String,
    pub ecosystem: Ecosystem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ecosystem {
    Rust,
    Npm,
}

/// Detect research topics by parsing the repo's dependency manifests.
fn detect_topics(root: &Path) -> Vec<Topic> {
    let mut topics: Vec<Topic> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    if let Ok(cargo) = std::fs::read_to_string(root.join("Cargo.toml")) {
        for name in parse_cargo_deps(&cargo) {
            if seen.insert((Ecosystem::Rust, name.clone())) {
                topics.push(Topic {
                    name,
                    ecosystem: Ecosystem::Rust,
                });
            }
        }
    }
    if let Ok(pkg) = std::fs::read_to_string(root.join("package.json")) {
        for name in parse_npm_deps(&pkg) {
            if seen.insert((Ecosystem::Npm, name.clone())) {
                topics.push(Topic {
                    name,
                    ecosystem: Ecosystem::Npm,
                });
            }
        }
    }
    topics
}

/// External crate names from a `Cargo.toml`'s `[dependencies]` and
/// `[workspace.dependencies]` tables. Path/workspace-local crates (those
/// with a `path = …`) are skipped — they're the repo itself, not something
/// to research.
fn parse_cargo_deps(toml_str: &str) -> Vec<String> {
    let Ok(doc) = toml::from_str::<toml::Table>(toml_str) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in ["dependencies", "dev-dependencies"] {
        if let Some(tbl) = doc.get(key).and_then(|v| v.as_table()) {
            collect_external_deps(tbl, &mut out);
        }
    }
    if let Some(tbl) = doc
        .get("workspace")
        .and_then(|v| v.as_table())
        .and_then(|ws| ws.get("dependencies"))
        .and_then(|v| v.as_table())
    {
        collect_external_deps(tbl, &mut out);
    }
    out
}

/// Push every non-path (external) dependency name from a deps table.
fn collect_external_deps(tbl: &toml::Table, out: &mut Vec<String>) {
    for (name, spec) in tbl {
        // Skip path/workspace-local deps (the repo's own crates).
        let is_local = spec
            .as_table()
            .map(|t| t.contains_key("path"))
            .unwrap_or(false);
        if !is_local {
            out.push(name.clone());
        }
    }
}

/// Package names from a `package.json`'s `dependencies` / `devDependencies`.
fn parse_npm_deps(pkg_json: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(pkg_json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in ["dependencies", "devDependencies"] {
        if let Some(obj) = value.get(key).and_then(|v| v.as_object()) {
            out.extend(obj.keys().cloned());
        }
    }
    out
}

/// Best-effort canonical documentation URL for a topic.
fn doc_url(topic: &Topic) -> String {
    match topic.ecosystem {
        Ecosystem::Rust => format!("https://docs.rs/{}", topic.name),
        Ecosystem::Npm => format!("https://www.npmjs.com/package/{}", topic.name),
    }
}

/// Markdown research plan for the agent to deepen with its deep-research
/// skill (real web search — blogs, changelogs, "latest" — that the binary
/// can't do itself).
fn render_research_plan(topics: &[Topic]) -> String {
    let mut s = String::from(
        "# Onboarding research plan\n\n\
         The binary crawled each dependency's docs into memory (wing=onboard).\n\
         Deepen these with the **deep-research** skill (web search across\n\
         blogs, changelogs, and latest releases), then feed findings back in\n\
         with `crabcc memory remember`:\n\n",
    );
    for t in topics {
        let eco = match t.ecosystem {
            Ecosystem::Rust => "rust crate",
            Ecosystem::Npm => "npm package",
        };
        s.push_str(&format!(
            "- **{}** ({eco}) — \"latest {} best practices, recent breaking changes, common pitfalls\" → {}\n",
            t.name, t.name, doc_url(t),
        ));
    }
    if topics.is_empty() {
        s.push_str("- _(no external dependencies detected)_\n");
    }
    s.push_str("\nThen: `crabcc enrich \"<topic>\"` pulls the cached docs as bounded context.\n");
    s
}

/// A lightweight codebase overview for the SessionStart injection.
fn render_overview(root: &Path, topics: &[Topic]) -> String {
    let mut s = String::from("# Codebase onboarding\n\n## Stack\n\n");
    if topics.is_empty() {
        s.push_str("- _(no dependency manifests detected)_\n");
    } else {
        for t in topics.iter().take(MAX_CRAWL_TOPICS) {
            s.push_str(&format!("- {}\n", t.name));
        }
    }
    s.push_str("\n## Top-level layout\n\n");
    for entry in top_level_dirs(root) {
        s.push_str(&format!("- `{entry}/`\n"));
    }
    s.push_str(
        "\n## Getting context\n\n\
         - `crabcc index` then `crabcc sym <Name>` / `crabcc outline <file>` for code.\n\
         - `crabcc enrich \"<topic>\"` for cached library docs (filled by the\n\
         background onboarding crawl; re-run as it completes).\n",
    );
    s
}

/// Visible top-level directory names (skips dotfiles + common noise),
/// sorted, capped for a compact overview.
fn top_level_dirs(root: &Path) -> Vec<String> {
    let mut dirs: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.') && n != "target" && n != "node_modules")
        .collect();
    dirs.sort();
    dirs.truncate(20);
    dirs
}

/// Spawn a detached background crawl per topic's doc URL. Returns how many
/// were launched. Failures to spawn are swallowed (best-effort onboarding).
fn spawn_background_crawls(topics: &[&Topic]) -> usize {
    let Ok(exe) = std::env::current_exe() else {
        return 0;
    };
    let mut n = 0;
    for t in topics {
        let ok = Command::new(&exe)
            .arg("crawl")
            .arg(doc_url(t))
            .arg("--remember")
            .args(["--depth", ONBOARD_DEPTH])
            .args(["--max-pages", ONBOARD_MAX_PAGES])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok();
        if ok {
            n += 1;
        }
    }
    n
}

/// SessionStart-hook snippet that injects the onboarding overview into the
/// agent's first prompts (paste into `.claude/settings.json`).
fn hook_snippet(root: &Path) -> String {
    let path = root.join(".crabcc").join("onboard").join("overview.md");
    format!(
        "To inject this on session start, add to .claude/settings.json:\n\
         {{\"hooks\":{{\"SessionStart\":[{{\"hooks\":[{{\"type\":\"command\",\
         \"command\":\"cat {}\"}}]}}]}}}}",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cargo_deps_skipping_local() {
        let toml = r#"
            [dependencies]
            serde = "1"
            tokio = { version = "1", features = ["full"] }
            crabcc-core = { path = "crates/crabcc-core" }
            [dev-dependencies]
            tempfile = "3"
            [workspace.dependencies]
            anyhow = "1"
        "#;
        let mut deps = parse_cargo_deps(toml);
        deps.sort();
        assert_eq!(deps, vec!["anyhow", "serde", "tempfile", "tokio"]);
        assert!(!deps.contains(&"crabcc-core".to_string())); // path dep skipped
    }

    #[test]
    fn parses_npm_deps() {
        let pkg = r#"{"dependencies":{"react":"^18"},"devDependencies":{"vite":"^5"}}"#;
        let mut deps = parse_npm_deps(pkg);
        deps.sort();
        assert_eq!(deps, vec!["react", "vite"]);
    }

    #[test]
    fn doc_urls_per_ecosystem() {
        assert_eq!(
            doc_url(&Topic {
                name: "tokio".into(),
                ecosystem: Ecosystem::Rust
            }),
            "https://docs.rs/tokio"
        );
        assert_eq!(
            doc_url(&Topic {
                name: "react".into(),
                ecosystem: Ecosystem::Npm
            }),
            "https://www.npmjs.com/package/react"
        );
    }

    #[test]
    fn research_plan_lists_topics_and_handles_empty() {
        let plan = render_research_plan(&[Topic {
            name: "serde".into(),
            ecosystem: Ecosystem::Rust,
        }]);
        assert!(plan.contains("**serde**"));
        assert!(plan.contains("docs.rs/serde"));
        assert!(render_research_plan(&[]).contains("no external dependencies"));
    }

    #[test]
    fn malformed_manifests_yield_no_topics() {
        assert!(parse_cargo_deps("this is not toml {{{").is_empty());
        assert!(parse_npm_deps("not json").is_empty());
    }
}
