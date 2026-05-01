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
//! HTML → Markdown via `htmd` (turndown.js-inspired, html5ever-backed,
//! Apache-2.0). Replaced `html2md` because that crate ships
//! `crate-type = ["rlib", "dylib", "staticlib"]`, and the `dylib` link
//! forces a `panic_unwind` runtime that conflicts with the workspace's
//! `panic = "abort"` release profile. Title
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
    if is_reddit_url(url) {
        return fetch_reddit_json(client, url).await;
    }
    let host = url_host(url).unwrap_or("");
    let prefer_main = host_uses_article_extractor(host);
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
                    let body_html = if prefer_main {
                        extract_main_content(&html).unwrap_or(&html)
                    } else {
                        &html
                    };
                    // We use `htmd` (turndown.js-inspired, Apache-2.0)
                    // instead of `html2md` because `html2md 0.2.x` ships
                    // `crate-type = ["rlib", "dylib", "staticlib"]`, and
                    // the `dylib` link forces a `panic_unwind` runtime
                    // that conflicts with the workspace's
                    // `panic = "abort"` release profile. `.skip_tags(...)`
                    // drops style/script/noscript content (html2md silently
                    // dropped those; htmd serializes them as text by
                    // default) so the output stays close to the prior shape.
                    let markdown = htmd::HtmlToMarkdown::builder()
                        .skip_tags(vec!["script", "style", "noscript"])
                        .build()
                        .convert(body_html)
                        .unwrap_or_default();
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

/// Domains where we prefer to extract `<article>` / `<main>` instead of
/// the full HTML — strips nav/footer/ads/sidebar noise. The fallback is
/// the original behaviour (whole-page HTML→Markdown via `htmd`) so unknown sites still
/// round-trip cleanly.
fn host_uses_article_extractor(host: &str) -> bool {
    matches!(
        host,
        "medium.com"
            | "www.medium.com"
            | "bbc.com"
            | "www.bbc.com"
            | "bbc.co.uk"
            | "www.bbc.co.uk"
            | "telex.hu"
            | "www.telex.hu"
    ) || host.ends_with(".medium.com")
}

/// Find the first `<article>` element; fall back to the first `<main>`;
/// fall back to None (caller uses the full HTML). Lowercase-tag matching;
/// nested same-tag elements are handled by counting depth.
fn extract_main_content(html: &str) -> Option<&str> {
    for tag in ["article", "main"] {
        if let Some(slice) = first_element(html, tag) {
            return Some(slice);
        }
    }
    None
}

fn first_element<'a>(html: &'a str, tag: &str) -> Option<&'a str> {
    let lower = html.to_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let start = lower.find(&open)?;
    // Skip past the opening tag's `>`.
    let after_open = html[start..].find('>')? + start + 1;
    // Walk forward respecting nested same-tag opens.
    let mut depth = 1usize;
    let mut cursor = after_open;
    while depth > 0 {
        let rest_lower = &lower[cursor..];
        let next_open = rest_lower.find(&open).map(|i| i + cursor);
        let next_close = rest_lower.find(&close).map(|i| i + cursor);
        match (next_open, next_close) {
            (Some(o), Some(c)) if o < c => {
                depth += 1;
                cursor = o + open.len();
            }
            (_, Some(c)) => {
                depth -= 1;
                if depth == 0 {
                    return Some(html[after_open..c].trim());
                }
                cursor = c + close.len();
            }
            _ => return None,
        }
    }
    None
}

fn is_reddit_url(url: &str) -> bool {
    matches!(
        url_host(url),
        Some("reddit.com" | "www.reddit.com" | "old.reddit.com" | "new.reddit.com")
    )
}

/// Fetch a Reddit URL via its `.json` API surface — orders-of-magnitude
/// less noise than scraping the HTML page, no JS required, gives us
/// title + body + author + comment count cleanly.
async fn fetch_reddit_json(client: &reqwest::Client, url: &str) -> FetchResult {
    let mut json_url = url.trim_end_matches('/').to_string();
    if let Some(q) = json_url.find('?') {
        json_url.truncate(q);
    }
    json_url.push_str(".json");

    match client.get(&json_url).send().await {
        Err(e) => FetchResult {
            url: url.into(),
            status: 0,
            title: None,
            content_markdown: None,
            via: Transport::Direct,
            error: Some(format!("reddit fetch: {e}")),
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
                    error: Some(format!("reddit body: {e}")),
                },
                Ok(body) => match render_reddit_json(&body) {
                    Some((title, md)) => FetchResult {
                        url: url.into(),
                        status,
                        title: Some(title),
                        content_markdown: Some(md),
                        via: Transport::Direct,
                        error: None,
                    },
                    None => FetchResult {
                        url: url.into(),
                        status,
                        title: None,
                        content_markdown: None,
                        via: Transport::Direct,
                        error: Some("reddit json had no recognisable post".into()),
                    },
                },
            }
        }
    }
}

/// Pull `(title, markdown)` out of a Reddit `.json` response. Reddit's
/// JSON shape is a 2-element array: `[listing_with_post, listing_with_comments]`.
/// We grab the post's `title` + `selftext`/`url` and the top N comment
/// bodies. Fail-soft — returns None on any shape surprise so the caller
/// can surface a clear error.
fn render_reddit_json(body: &str) -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let post = v
        .as_array()?
        .first()?
        .get("data")?
        .get("children")?
        .as_array()?
        .first()?
        .get("data")?;
    let title = post.get("title")?.as_str()?.to_string();
    let author = post.get("author").and_then(|a| a.as_str()).unwrap_or("?");
    let subreddit = post
        .get("subreddit")
        .and_then(|s| s.as_str())
        .unwrap_or("?");
    let selftext = post
        .get("selftext")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim();
    let post_url = post.get("url").and_then(|u| u.as_str()).unwrap_or("");
    let score = post.get("score").and_then(|s| s.as_i64()).unwrap_or(0);

    let mut md = String::new();
    md.push_str(&format!(
        "**r/{subreddit}**  \\| u/{author} \\| score {score}\n\n"
    ));
    if !selftext.is_empty() {
        md.push_str(selftext);
        md.push_str("\n\n");
    } else if !post_url.is_empty() {
        md.push_str(&format!("link: {post_url}\n\n"));
    }

    // Comments — top 5 first-level only, no recursion (tight token budget).
    if let Some(comments) = v
        .as_array()
        .and_then(|a| a.get(1))
        .and_then(|c| c.get("data"))
        .and_then(|d| d.get("children"))
        .and_then(|c| c.as_array())
    {
        md.push_str("---\n\n## Top comments\n\n");
        for c in comments.iter().take(5) {
            let data = match c.get("data") {
                Some(d) => d,
                None => continue,
            };
            let cauthor = data.get("author").and_then(|a| a.as_str()).unwrap_or("?");
            let cscore = data.get("score").and_then(|s| s.as_i64()).unwrap_or(0);
            let cbody = data
                .get("body")
                .and_then(|b| b.as_str())
                .unwrap_or("")
                .trim();
            if cbody.is_empty() {
                continue;
            }
            md.push_str(&format!("**u/{cauthor}** ({cscore}):\n{cbody}\n\n"));
        }
    }

    Some((title, md))
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

    #[test]
    fn host_uses_article_extractor_picks_known_sites() {
        for h in [
            "medium.com",
            "www.medium.com",
            "uxdesign.cc.medium.com",
            "bbc.com",
            "www.bbc.co.uk",
            "telex.hu",
            "www.telex.hu",
        ] {
            assert!(
                host_uses_article_extractor(h),
                "expected article-extractor host: {h}"
            );
        }
        for h in ["example.com", "rust-lang.org", "github.com"] {
            assert!(
                !host_uses_article_extractor(h),
                "did not expect article-extractor host: {h}"
            );
        }
    }

    #[test]
    fn extract_main_content_picks_article() {
        let html = "<html><body><nav>x</nav><article><p>hello</p></article><footer>z</footer></body></html>";
        assert_eq!(extract_main_content(html), Some("<p>hello</p>"));
    }

    #[test]
    fn extract_main_content_falls_back_to_main() {
        let html = "<html><body><main>body content</main></body></html>";
        assert_eq!(extract_main_content(html), Some("body content"));
    }

    #[test]
    fn extract_main_content_returns_none_when_neither_present() {
        let html = "<html><body><div>just a div</div></body></html>";
        assert_eq!(extract_main_content(html), None);
    }

    #[test]
    fn extract_main_content_handles_nested_articles() {
        let html = "<article>outer <article>nested</article> outer-tail</article>";
        assert_eq!(
            extract_main_content(html),
            Some("outer <article>nested</article> outer-tail")
        );
    }

    #[test]
    fn is_reddit_url_recognises_known_hosts() {
        assert!(is_reddit_url("https://reddit.com/r/rust/comments/abc/foo"));
        assert!(is_reddit_url("https://www.reddit.com/r/rust"));
        assert!(is_reddit_url("https://old.reddit.com/r/rust"));
        assert!(!is_reddit_url("https://reddit-clone.example.com/foo"));
    }

    #[test]
    fn render_reddit_json_extracts_title_and_body() {
        let body = serde_json::json!([
            {"data": {"children": [{"data": {
                "title": "Hello world",
                "author": "alice",
                "subreddit": "rust",
                "selftext": "Body of post",
                "url": "https://reddit.com/r/rust/comments/abc/hello_world",
                "score": 42,
            }}]}},
            {"data": {"children": [
                {"data": {"author": "bob", "score": 7, "body": "first comment"}},
                {"data": {"author": "carol", "score": 3, "body": "second"}},
            ]}}
        ])
        .to_string();
        let (title, md) = render_reddit_json(&body).expect("should render");
        assert_eq!(title, "Hello world");
        assert!(md.contains("**r/rust**"));
        assert!(md.contains("u/alice"));
        assert!(md.contains("Body of post"));
        assert!(md.contains("u/bob"));
        assert!(md.contains("first comment"));
    }

    #[test]
    fn render_reddit_json_returns_none_on_garbage() {
        assert!(render_reddit_json("not json").is_none());
        assert!(render_reddit_json("{}").is_none());
        assert!(render_reddit_json("[]").is_none());
    }
}
