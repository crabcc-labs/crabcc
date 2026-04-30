//! `crabcc fetch` — extract URLs from prose, fetch in parallel, return
//! cleaned-to-markdown content. Designed to feed agent-loop prompts that
//! reference URLs without the agent having to make tool calls per URL.
//!
//! Two transport paths:
//!   1. **Chrome bridge** (preferred when a paired extension is online).
//!      Routes through `http://localhost:7878/api/chrome-bridge/status`
//!      → `page.fetch` MCP method → user's authenticated browser session.
//!      Currently a stub — falls back to direct until the chrome-bridge
//!      endpoints from #184 land in `crabcc-viz`.
//!   2. **Direct HTTP** (fallback). `reqwest` with a sensible User-Agent
//!      and 20-second per-URL timeout. Battle-tested deps only.
//!
//! HTML → Markdown via `html2md` (html5ever-backed, mature). Title
//! extraction via a minimal `<title>` scan to avoid pulling in a DOM lib
//! for one element.

use anyhow::Result;
use crabcc_memory::Palace;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

const USER_AGENT: &str = concat!("crabcc-fetch/", env!("CARGO_PKG_VERSION"));
const PER_URL_TIMEOUT: Duration = Duration::from_secs(20);
const CHROME_BRIDGE_STATUS: &str = "http://localhost:7878/api/chrome-bridge/status";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResult {
    pub url: String,
    pub status: u16,
    pub title: Option<String>,
    pub content_markdown: Option<String>,
    pub via: Transport,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Direct,
    Chrome,
    /// GitHub repo packed via `repomix --compress --remote …` instead of
    /// HTML scrape — orders-of-magnitude smaller for code repos.
    Repomix,
}

/// Repomix ignore patterns for GitHub repo packing. README.md is intentionally
/// kept (repomix's defaults preserve it). The list aggressively trims
/// token-bloat: build manifests, lockfiles, CI workflows, other markdown
/// (which is usually contributor docs / architecture notes), and binaries
/// repomix would skip anyway but listed here for clarity.
const REPOMIX_IGNORE: &str = concat!(
    "**/*.md,!README.md,!readme.md,!Readme.md,",
    "**/Cargo.toml,**/Cargo.lock,",
    "**/package.json,**/package-lock.json,**/yarn.lock,**/bun.lock,**/bun.lockb,**/pnpm-lock.yaml,",
    "**/pyproject.toml,**/Pipfile,**/Pipfile.lock,**/requirements*.txt,**/setup.py,**/setup.cfg,",
    "**/Gemfile,**/Gemfile.lock,**/go.sum,**/go.mod,",
    ".github/**,.gitlab-ci.yml,.circleci/**,.travis.yml,.drone.yml,Jenkinsfile,",
    "LICENSE*,COPYING*,CHANGELOG*,CHANGES*,CONTRIBUTING*,CODE_OF_CONDUCT*,",
    "**/*.svg,**/*.png,**/*.jpg,**/*.jpeg,**/*.gif,**/*.ico,**/*.webp",
);

/// Extract HTTP/HTTPS URLs from arbitrary prose using `linkify`. Order
/// preserved; duplicates removed by stable de-dup so the caller sees each
/// URL exactly once even if a prompt mentions it twice.
pub fn extract_urls(prompt: &str) -> Vec<String> {
    let mut finder = linkify::LinkFinder::new();
    finder.kinds(&[linkify::LinkKind::Url]);
    let mut seen = std::collections::HashSet::new();
    finder
        .links(prompt)
        .map(|l| l.as_str().to_string())
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        .filter(|u| seen.insert(u.clone()))
        .collect()
}

/// Public entry — runs the fetch under a current-thread tokio runtime so
/// the CLI handler stays sync. Mirrors the pattern in `jobs_cmd.rs`.
///
/// `remember` pipes each successful fetch into `<root>/.crabcc/memory.db`
/// (BM25 ⊕ vector embedding) under wing="fetch", room=host, source_id=url.
pub fn run(root: &Path, prompt: &str, no_chrome: bool, format: &str, remember: bool) -> Result<()> {
    let urls = extract_urls(prompt);
    if urls.is_empty() {
        anyhow::bail!("no URLs found in prompt");
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let chrome_default = !no_chrome;
    let results: Vec<FetchResult> = rt.block_on(async move {
        let chrome_ok = chrome_default && chrome_bridge_available().await;
        if chrome_ok {
            fetch_via_chrome(&urls).await
        } else {
            fetch_direct(&urls).await
        }
    });

    if remember {
        store_in_memory(root, &results)?;
    }

    match format {
        "text" => render_text(&results),
        _ => {
            let json = serde_json::to_string_pretty(&results)?;
            println!("{json}");
        }
    }
    Ok(())
}

fn store_in_memory(root: &Path, results: &[FetchResult]) -> Result<()> {
    let palace = Palace::open(root)?;
    let session = std::env::var("TERM_SESSION_ID").ok();
    for r in results {
        if r.error.is_some() || r.content_markdown.is_none() {
            continue;
        }
        let host = url_host(&r.url).unwrap_or("unknown");
        let title = r.title.as_deref().unwrap_or("");
        let body = if title.is_empty() {
            r.content_markdown.clone().unwrap_or_default()
        } else {
            format!(
                "# {title}\n\n{}",
                r.content_markdown.as_deref().unwrap_or("")
            )
        };
        let _ = palace.remember_in_session("fetch", Some(host), &r.url, &body, session.as_deref());
    }
    Ok(())
}

fn url_host(url: &str) -> Option<&str> {
    let after_scheme = url.split_once("://")?.1;
    let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    let host = &after_scheme[..host_end];
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn render_text(results: &[FetchResult]) {
    for r in results {
        println!("# {}", r.url);
        if let Some(t) = &r.title {
            println!("**title:** {t}");
        }
        println!("**status:** {} (via {:?})", r.status, r.via);
        if let Some(e) = &r.error {
            println!("**error:** {e}\n");
            continue;
        }
        if let Some(md) = &r.content_markdown {
            println!("\n{md}\n");
        }
        println!("---");
    }
}

async fn chrome_bridge_available() -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    matches!(
        client.get(CHROME_BRIDGE_STATUS).send().await,
        Ok(r) if r.status().is_success()
    )
}

async fn fetch_via_chrome(urls: &[String]) -> Vec<FetchResult> {
    // Stub — chrome-bridge endpoints land with #184's broker work in
    // crabcc-viz. Until then this falls through to the direct path so
    // the CLI is fully functional in either world.
    let mut results = fetch_direct(urls).await;
    for r in &mut results {
        r.via = Transport::Direct;
    }
    results
}

async fn fetch_direct(urls: &[String]) -> Vec<FetchResult> {
    let client = match reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(PER_URL_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return urls
                .iter()
                .map(|u| FetchResult {
                    url: u.clone(),
                    status: 0,
                    title: None,
                    content_markdown: None,
                    via: Transport::Direct,
                    error: Some(format!("client build failed: {e}")),
                })
                .collect();
        }
    };

    let mut set = tokio::task::JoinSet::new();
    for url in urls {
        let client = client.clone();
        let url = url.clone();
        set.spawn(async move { fetch_one(&client, &url).await });
    }

    let mut out = Vec::with_capacity(urls.len());
    while let Some(r) = set.join_next().await {
        out.push(r.unwrap_or_else(|e| FetchResult {
            url: String::new(),
            status: 0,
            title: None,
            content_markdown: None,
            via: Transport::Direct,
            error: Some(format!("join error: {e}")),
        }));
    }
    out
}

async fn fetch_one(client: &reqwest::Client, url: &str) -> FetchResult {
    if let Some(repo_coord) = parse_github_repo(url) {
        return fetch_via_repomix(url, &repo_coord).await;
    }
    match client.get(url).send().await {
        Err(e) => FetchResult {
            url: url.into(),
            status: 0,
            title: None,
            content_markdown: None,
            via: Transport::Direct,
            error: Some(e.to_string()),
        },
        Ok(resp) => {
            let status = resp.status().as_u16();
            match resp.text().await {
                Err(e) => FetchResult {
                    url: url.into(),
                    status,
                    title: None,
                    content_markdown: None,
                    via: Transport::Direct,
                    error: Some(e.to_string()),
                },
                Ok(html) => {
                    let title = extract_title(&html);
                    let markdown = html2md::parse_html(&html);
                    FetchResult {
                        url: url.into(),
                        status,
                        title,
                        content_markdown: Some(markdown),
                        via: Transport::Direct,
                        error: None,
                    }
                }
            }
        }
    }
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let tag_start = lower.find("<title")?;
    let body_start = html[tag_start..].find('>')? + tag_start + 1;
    let end = lower[body_start..].find("</title>")? + body_start;
    let raw = html[body_start..end].trim();
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

/// Recognise canonical GitHub repo URLs and return `owner/repo`.
/// Handles `/tree/branch`, `/blob/path`, and `.git` suffixes by trimming
/// to the first two path segments. Returns `None` for issues, gist,
/// pulls, anything that's not a plain repo root pointer.
pub fn parse_github_repo(url: &str) -> Option<String> {
    // Reserved top-level paths on github.com that aren't repos.
    const RESERVED_OWNERS: &[&str] = &[
        "sponsors",
        "marketplace",
        "orgs",
        "topics",
        "explore",
        "notifications",
        "settings",
        "features",
        "pricing",
        "about",
        "join",
        "login",
        "logout",
        "search",
        "trending",
        "collections",
        "events",
        "stars",
        "issues",
        "pulls",
        "watching",
        "new",
        "organizations",
    ];
    let url = url.trim();
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let mut segs = path.split('/');
    let owner = segs.next()?;
    let repo = segs.next()?;
    if owner.is_empty() || repo.is_empty() || RESERVED_OWNERS.contains(&owner) {
        return None;
    }
    // Repos can have any name, but the third segment should be tree/blob/raw
    // or absent for it to be a repo root pointer (not /issues, /pull, …).
    if let Some(third) = segs.next() {
        match third {
            "tree" | "blob" | "raw" | "" => {}
            _ => return None,
        }
    }
    Some(format!("{owner}/{repo}"))
}

async fn fetch_via_repomix(orig_url: &str, coord: &str) -> FetchResult {
    let coord_owned = coord.to_string();
    let coord_for_task = coord_owned.clone();
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new("repomix")
            .args([
                "--remote",
                &coord_for_task,
                "--compress",
                "--stdout",
                "--no-security-check",
                "--ignore",
                REPOMIX_IGNORE,
            ])
            .output()
    })
    .await;
    let coord = coord_owned;

    match out {
        Ok(Ok(o)) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
            FetchResult {
                url: orig_url.into(),
                status: 200,
                title: Some(format!("github:{coord} (repomix --compress)")),
                content_markdown: Some(stdout),
                via: Transport::Repomix,
                error: None,
            }
        }
        Ok(Ok(o)) => FetchResult {
            url: orig_url.into(),
            status: 0,
            title: None,
            content_markdown: None,
            via: Transport::Repomix,
            error: Some(format!(
                "repomix exited {}: {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            )),
        },
        Ok(Err(e)) => FetchResult {
            url: orig_url.into(),
            status: 0,
            title: None,
            content_markdown: None,
            via: Transport::Repomix,
            error: Some(format!("repomix spawn failed: {e} (is `repomix` on PATH?)")),
        },
        Err(e) => FetchResult {
            url: orig_url.into(),
            status: 0,
            title: None,
            content_markdown: None,
            via: Transport::Repomix,
            error: Some(format!("blocking task join error: {e}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_urls_in_order_dedup() {
        let prompt =
            "see https://a.example/x and http://b.example/y, also https://a.example/x again";
        let urls = extract_urls(prompt);
        assert_eq!(
            urls,
            vec![
                "https://a.example/x".to_string(),
                "http://b.example/y".to_string()
            ]
        );
    }

    #[test]
    fn extracts_urls_ignores_non_http_schemes() {
        let prompt = "ftp://nope and https://yes.example";
        let urls = extract_urls(prompt);
        assert_eq!(urls, vec!["https://yes.example".to_string()]);
    }

    #[test]
    fn extracts_urls_returns_empty_for_pure_prose() {
        assert!(extract_urls("just words, no URLs here").is_empty());
    }

    #[test]
    fn title_extraction_basic() {
        let html = r#"<html><head><title>Hello World</title></head><body>x</body></html>"#;
        assert_eq!(extract_title(html).as_deref(), Some("Hello World"));
    }

    #[test]
    fn title_extraction_with_attrs() {
        let html = r#"<title lang="en">Tagged</title>"#;
        assert_eq!(extract_title(html).as_deref(), Some("Tagged"));
    }

    #[test]
    fn title_extraction_missing() {
        assert_eq!(extract_title("<html><body>no title</body></html>"), None);
    }

    #[test]
    fn parse_github_repo_canonical() {
        assert_eq!(
            parse_github_repo("https://github.com/owner/repo").as_deref(),
            Some("owner/repo")
        );
        assert_eq!(
            parse_github_repo("https://github.com/owner/repo/").as_deref(),
            Some("owner/repo")
        );
        assert_eq!(
            parse_github_repo("https://github.com/owner/repo.git").as_deref(),
            Some("owner/repo")
        );
    }

    #[test]
    fn parse_github_repo_with_tree() {
        assert_eq!(
            parse_github_repo("https://github.com/rust-lang/rust/tree/master").as_deref(),
            Some("rust-lang/rust")
        );
    }

    #[test]
    fn parse_github_repo_rejects_non_repo_paths() {
        assert_eq!(
            parse_github_repo("https://github.com/owner/repo/issues/42"),
            None
        );
        assert_eq!(
            parse_github_repo("https://github.com/owner/repo/pull/1"),
            None
        );
        assert_eq!(parse_github_repo("https://github.com/sponsors/owner"), None);
        assert_eq!(parse_github_repo("https://example.com/owner/repo"), None);
    }

    #[test]
    fn url_host_extracts_authority() {
        assert_eq!(url_host("https://example.com/path"), Some("example.com"));
        assert_eq!(url_host("https://example.com"), Some("example.com"));
        assert_eq!(url_host("http://localhost:7878/x"), Some("localhost:7878"));
        assert_eq!(url_host("not-a-url"), None);
    }
}
