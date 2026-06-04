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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// How many dependency doc-sites to crawl in the background. Kept modest so
/// onboarding is "medium-sized", not a full mirror.
const MAX_CRAWL_TOPICS: usize = 12;
/// Per-topic crawl budget for the background onboarding crawls.
const ONBOARD_DEPTH: &str = "1";
const ONBOARD_MAX_PAGES: &str = "15";

/// A lock with no `done` marker older than this is treated as a crashed
/// leader and taken over.
const LOCK_STALE_SECS: u64 = 600;
/// How long a follower waits for the leader to publish artifacts before
/// reusing whatever's there. The leader only needs to write files + spawn
/// the crawls (fast); it does NOT hold this until the crawls finish.
const WAIT_TIMEOUT: Duration = Duration::from_secs(45);
const WAIT_POLL: Duration = Duration::from_millis(500);

/// Memory wing the deep-research handoff stores findings into — separate
/// from crawl docs so research can be queried (and enriched) on its own.
const RESULT_WING: &str = "research";
/// How many SessionStart injections to emit before going silent: the
/// "first few prompts" kickstart, after which onboarding context tapers off.
const INJECT_LIMIT: u32 = 3;

/// Onboard the repo — **deduped across concurrent agents**.
///
/// Launch N agents at once and exactly one wins an atomic lock and becomes
/// the leader: it does the research + spawns the background doc-crawls once,
/// then publishes a `done` marker. The other agents detect the in-flight
/// (or finished) onboarding and reuse its artifacts instead of each firing
/// off their own crawl storm. A crashed leader's stale lock is taken over.
pub fn run(root: &Path) -> Result<()> {
    let onboard_dir = root.join(".crabcc").join("onboard");
    std::fs::create_dir_all(&onboard_dir).context("create .crabcc/onboard")?;
    let lock = onboard_dir.join(".lock");
    let done = onboard_dir.join(".done");

    // Bounded loop so a stale-lock takeover retries without spinning.
    for _ in 0..3 {
        if done.exists() {
            return reuse(&onboard_dir, "already onboarded");
        }
        match try_acquire(&lock)? {
            true => {
                // Leader: onboard exactly once. Publish `done` ONLY on
                // success — a failed lead() must not mark the repo
                // "onboarded". On failure, release the lock so another agent
                // retries instead of waiting out the stale TTL.
                return match lead(root, &onboard_dir) {
                    Ok(()) => {
                        let _ = std::fs::write(&done, unix_now().to_string());
                        Ok(())
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&lock);
                        Err(e)
                    }
                };
            }
            false => {
                // Another agent holds the lock. If it looks crashed (stale,
                // no done marker), take it over — but only if it's still the
                // *same* stale lock we inspected, so we never unlink a fresh
                // leader's lock that just replaced it (which would split
                // leadership). `create_new` remains the real election, so
                // even a lost race here resolves to a single leader.
                if let Some(mtime) = stale_lock_mtime(&lock, LOCK_STALE_SECS) {
                    let unchanged =
                        std::fs::metadata(&lock).and_then(|m| m.modified()).ok() == Some(mtime);
                    if unchanged {
                        let _ = std::fs::remove_file(&lock);
                    }
                    continue;
                }
                let finished = wait_for(&done, WAIT_TIMEOUT);
                return reuse(
                    &onboard_dir,
                    if finished {
                        "onboarding completed by another agent"
                    } else {
                        "onboarding already in progress elsewhere"
                    },
                );
            }
        }
    }
    reuse(&onboard_dir, "reusing existing onboarding")
}

/// The actual onboarding work — run by the lock leader only.
fn lead(root: &Path, onboard_dir: &Path) -> Result<()> {
    let topics = detect_topics(root);
    if topics.is_empty() {
        eprintln!(
            "init: no dependency manifests found under {} — nothing to research yet",
            root.display()
        );
    }

    std::fs::write(
        onboard_dir.join("research-plan.md"),
        render_research_plan(&topics),
    )?;
    std::fs::write(
        onboard_dir.join("research-plan.json"),
        render_research_plan_json(&topics),
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
        "init: wrote {}/research-plan.{{md,json}} and overview.md",
        onboard_dir.display()
    );
    eprintln!("\n{}", hook_snippet());
    Ok(())
}

/// SessionStart-hook entry point (`crabcc init --inject`): emit the
/// onboarding context for the first [`INJECT_LIMIT`] invocations, then go
/// silent so it tapers off after the agent is oriented. Cheap — reads the
/// overview the leader wrote; never crawls, locks, or blocks.
pub fn run_inject(root: &Path) -> Result<()> {
    // The hook may launch from a workspace subdirectory (root resolves to
    // the cwd, which doesn't walk up), so search ancestors for the onboarded
    // dir. None up-tree → not onboarded here → stay silent.
    let Some(onboard) = find_onboard_dir(root) else {
        return Ok(());
    };
    let counter = onboard.join(".injected");
    let count = std::fs::read_to_string(&counter)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    if count >= INJECT_LIMIT {
        return Ok(()); // oriented — stay silent
    }
    // The overview always exists here (find_onboard_dir requires it), but
    // guard anyway and only consume a slot when we actually emit.
    if let Ok(text) = std::fs::read_to_string(onboard.join("overview.md")) {
        print!("{text}");
        let _ = std::fs::write(&counter, (count + 1).to_string());
    }
    Ok(())
}

/// Walk up from `start` to the nearest ancestor whose
/// `.crabcc/onboard/overview.md` exists, returning that onboard dir. Lets
/// `--inject` find onboarding even when launched from a subdirectory.
fn find_onboard_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let onboard = d.join(".crabcc").join("onboard");
        if onboard.join("overview.md").is_file() {
            return Some(onboard);
        }
        dir = d.parent();
    }
    None
}

/// Atomically try to become the onboarding leader. `Ok(true)` = we created
/// the lock (lead); `Ok(false)` = another agent holds it. The `create_new`
/// open is the atomic primitive that makes exactly one agent win.
fn try_acquire(lock: &Path) -> Result<bool> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock)
    {
        Ok(mut f) => {
            use std::io::Write as _;
            let _ = writeln!(f, "{} pid={}", unix_now(), std::process::id());
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(anyhow::Error::new(e).context("create onboarding lock")),
    }
}

/// If `lock` exists and is older than `ttl` seconds, return its mtime — so a
/// takeover can confirm the lock is unchanged before unlinking it. `None`
/// when the lock is missing or still fresh.
fn stale_lock_mtime(lock: &Path, ttl: u64) -> Option<std::time::SystemTime> {
    let mtime = std::fs::metadata(lock).ok()?.modified().ok()?;
    if mtime.elapsed().ok()?.as_secs() > ttl {
        Some(mtime)
    } else {
        None
    }
}

/// Block until `path` exists or `timeout` elapses; returns whether it
/// appeared.
fn wait_for(path: &Path, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if path.exists() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(WAIT_POLL);
    }
}

/// A follower's path: point at the artifacts the leader produced (they keep
/// filling as the background crawls land).
fn reuse(onboard_dir: &Path, why: &str) -> Result<()> {
    eprintln!(
        "init: {why} — reusing {}/overview.md (docs fill in via the background \
         crawl; `crabcc enrich \"<topic>\"` as they land)",
        onboard_dir.display()
    );
    Ok(())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

/// One research task in the machine-readable handoff.
#[derive(serde::Serialize)]
struct ResearchTask {
    topic: String,
    ecosystem: &'static str,
    doc_url: String,
    queries: Vec<String>,
}

/// The machine-readable research plan the deep-research skill iterates.
#[derive(serde::Serialize)]
struct ResearchPlan {
    /// Where to store findings: `crabcc memory remember --wing <result_wing>
    /// --room <topic> …`, then retrieve via `crabcc enrich`.
    result_wing: &'static str,
    tasks: Vec<ResearchTask>,
}

/// Render the research plan as JSON — one task per dependency with stable
/// search queries + the result wing. Pairs with the human research-plan.md;
/// this is the traceable handoff an agent can drive programmatically.
fn render_research_plan_json(topics: &[Topic]) -> String {
    let tasks = topics
        .iter()
        .map(|t| ResearchTask {
            topic: t.name.clone(),
            ecosystem: match t.ecosystem {
                Ecosystem::Rust => "rust",
                Ecosystem::Npm => "npm",
            },
            doc_url: doc_url(t),
            queries: vec![
                format!(
                    "{} latest stable release notes and breaking changes",
                    t.name
                ),
                format!("{} common pitfalls and best practices", t.name),
            ],
        })
        .collect();
    let plan = ResearchPlan {
        result_wing: RESULT_WING,
        tasks,
    };
    serde_json::to_string_pretty(&plan).unwrap_or_else(|_| "{}".into())
}

/// A lightweight codebase overview for the SessionStart injection.
fn render_overview(root: &Path, topics: &[Topic]) -> String {
    let mut s = String::from("# Codebase onboarding\n\n");

    if let Some(desc) = std::fs::read_to_string(root.join("README.md"))
        .ok()
        .and_then(|t| readme_summary(&t))
    {
        s.push_str("## What this is\n\n");
        s.push_str(&desc);
        s.push_str("\n\n");
    }

    s.push_str("## Stack\n\n");
    if topics.is_empty() {
        s.push_str("- _(no dependency manifests detected)_\n");
    } else {
        for t in topics.iter().take(MAX_CRAWL_TOPICS) {
            s.push_str(&format!("- {}\n", t.name));
        }
    }

    s.push_str("\n## Entry points\n\n");
    let eps = entry_points(root);
    if eps.is_empty() {
        s.push_str("- _(none detected)_\n");
    } else {
        for e in &eps {
            s.push_str(&format!("- `{e}`\n"));
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

/// First descriptive paragraph of a README: skip a leading H1 / badges /
/// blockquotes / HTML / blank lines, then take the first run of prose lines,
/// truncated for the overview.
fn readme_summary(text: &str) -> Option<String> {
    let mut para = String::new();
    // True while inside a multi-line HTML block (e.g. a centered <h1>…</h1>
    // header or <p>…</p> badge block) whose inner text lines don't
    // themselves start with `<` and would otherwise be mistaken for prose.
    let mut in_html = false;
    for line in text.lines() {
        let t = line.trim();
        if !para.is_empty() {
            if t.is_empty() {
                break; // end of the first paragraph
            }
            para.push(' ');
            para.push_str(t);
            continue;
        }
        // Still scanning for the start of the first prose paragraph.
        if in_html {
            // Skip the whole block until it closes or a blank line ends it.
            if t.is_empty() || t.contains("</") || t.ends_with("/>") {
                in_html = false;
            }
            continue;
        }
        if t.is_empty()
            || t.starts_with('#')
            || t.starts_with("![")
            || t.starts_with('[')
            || t.starts_with('>')
        {
            continue;
        }
        if t.starts_with('<') {
            // Enter block-skip mode unless this tag is closed on its own line.
            if !(t.contains("</") || t.ends_with("/>")) {
                in_html = true;
            }
            continue;
        }
        para.push_str(t);
    }
    if para.is_empty() {
        None
    } else {
        Some(truncate(&para, 400))
    }
}

/// Likely program entry points at conventional locations (cheap, no full
/// walk): repo-root `src/{main,lib}.rs` + JS index files, plus each
/// `crates/<name>/src/{main,lib}.rs` in a Cargo workspace.
fn entry_points(root: &Path) -> Vec<String> {
    let mut eps = Vec::new();
    for rel in [
        "src/main.rs",
        "src/lib.rs",
        "src/index.ts",
        "src/index.js",
        "index.js",
    ] {
        if root.join(rel).is_file() {
            eps.push(rel.to_string());
        }
    }
    if let Ok(rd) = std::fs::read_dir(root.join("crates")) {
        let mut names: Vec<String> = rd
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        for c in names {
            for f in ["main.rs", "lib.rs"] {
                let rel = format!("crates/{c}/src/{f}");
                if root.join(&rel).is_file() {
                    eps.push(rel);
                }
            }
        }
    }
    eps.truncate(30);
    eps
}

/// Truncate to `max` bytes on a char boundary, appending an ellipsis.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
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
fn hook_snippet() -> String {
    "To inject onboarding context into your first few prompts, add to \
     .claude/settings.json:\n\
     {\"hooks\":{\"SessionStart\":[{\"hooks\":[{\"type\":\"command\",\
     \"command\":\"crabcc init --inject\"}]}]}}"
        .to_string()
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

    #[test]
    fn readme_summary_skips_chrome_and_takes_first_paragraph() {
        let md = "# crabcc\n\n![badge](x) [link](y)\n\n> a tagline quote\n\n\
                  The fast symbol index.\nLine two of the para.\n\nNext paragraph ignored.\n";
        assert_eq!(
            readme_summary(md).unwrap(),
            "The fast symbol index. Line two of the para."
        );
        assert!(readme_summary("# only a title\n").is_none());
    }

    #[test]
    fn readme_summary_skips_multiline_html_header_block() {
        // Inner text (`crabcc`) of a multi-line <h1> must not become the
        // paragraph — the whole block is skipped until it closes.
        let md = "<h1 align=\"center\">\ncrabcc\n</h1>\n\n\
                  <p><img src=\"x\"></p>\n\nReal description here.\n";
        assert_eq!(readme_summary(md).unwrap(), "Real description here.");
    }

    #[test]
    fn truncate_adds_ellipsis_on_char_boundary() {
        assert_eq!(truncate("short", 100), "short");
        assert_eq!(truncate("héllo", 2), "h…"); // mid 'é' → backs up to 'h'
    }

    #[test]
    fn lock_is_exclusive_so_one_agent_leads() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join(".lock");
        assert!(try_acquire(&lock).unwrap()); // first agent leads
        assert!(!try_acquire(&lock).unwrap()); // concurrent agent sees it held
        assert!(!try_acquire(&lock).unwrap());
    }

    #[test]
    fn done_marker_short_circuits_without_leading() {
        let dir = tempfile::tempdir().unwrap();
        let onboard = dir.path().join(".crabcc").join("onboard");
        std::fs::create_dir_all(&onboard).unwrap();
        std::fs::write(onboard.join(".done"), "1").unwrap();
        // Already onboarded → reuse path; must NOT take the lock or spawn.
        run(dir.path()).unwrap();
        assert!(!onboard.join(".lock").exists());
    }

    #[test]
    fn wait_for_returns_false_on_timeout() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!wait_for(
            &dir.path().join("nope"),
            Duration::from_millis(40)
        ));
    }

    #[test]
    fn research_plan_json_carries_wing_and_tasks() {
        let json = render_research_plan_json(&[Topic {
            name: "serde".into(),
            ecosystem: Ecosystem::Rust,
        }]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["result_wing"], "research");
        assert_eq!(v["tasks"][0]["topic"], "serde");
        assert_eq!(v["tasks"][0]["ecosystem"], "rust");
        assert!(!v["tasks"][0]["queries"].as_array().unwrap().is_empty());
    }

    #[test]
    fn inject_emits_first_few_then_taps_off() {
        let dir = tempfile::tempdir().unwrap();
        let onboard = dir.path().join(".crabcc").join("onboard");
        std::fs::create_dir_all(&onboard).unwrap();
        std::fs::write(onboard.join("overview.md"), "ctx").unwrap();
        for _ in 0..INJECT_LIMIT + 2 {
            run_inject(dir.path()).unwrap();
        }
        let count: u32 = std::fs::read_to_string(onboard.join(".injected"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(count, INJECT_LIMIT); // capped — tapered off after first few
    }

    #[test]
    fn inject_is_silent_without_an_overview() {
        let dir = tempfile::tempdir().unwrap();
        run_inject(dir.path()).unwrap(); // no onboarding artifacts yet
        assert!(!dir.path().join(".crabcc/onboard/.injected").exists());
    }

    #[test]
    fn inject_finds_onboarding_from_a_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let onboard = dir.path().join(".crabcc").join("onboard");
        std::fs::create_dir_all(&onboard).unwrap();
        std::fs::write(onboard.join("overview.md"), "ctx").unwrap();
        let subdir = dir.path().join("crates").join("foo");
        std::fs::create_dir_all(&subdir).unwrap();
        // Launched from a subdir, inject still finds the ancestor onboarding.
        run_inject(&subdir).unwrap();
        let count: u32 = std::fs::read_to_string(onboard.join(".injected"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn entry_points_finds_root_and_workspace_crates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "").unwrap();
        std::fs::create_dir_all(root.join("crates/foo/src")).unwrap();
        std::fs::write(root.join("crates/foo/src/main.rs"), "").unwrap();
        let eps = entry_points(root);
        assert!(eps.contains(&"src/lib.rs".to_string()));
        assert!(eps.contains(&"crates/foo/src/main.rs".to_string()));
    }
}
