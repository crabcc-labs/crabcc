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
use serde::{Deserialize, Serialize};
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
}

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
pub fn run(prompt: &str, no_chrome: bool, format: &str) -> Result<()> {
    let urls = extract_urls(prompt);
    if urls.is_empty() {
        anyhow::bail!("no URLs found in prompt");
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let results: Vec<FetchResult> = rt.block_on(async move {
        let transport = if no_chrome {
            Transport::Direct
        } else if chrome_bridge_available().await {
            Transport::Chrome
        } else {
            Transport::Direct
        };

        match transport {
            Transport::Chrome => fetch_via_chrome(&urls).await,
            Transport::Direct => fetch_direct(&urls).await,
        }
    });

    match format {
        "text" => render_text(&results),
        _ => {
            let json = serde_json::to_string_pretty(&results)?;
            println!("{json}");
        }
    }
    Ok(())
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
}
