//! `crabcc-fetch` — shared URL extraction + HTTP fetch + HTML→Markdown
//! cleaning helpers used by both the CLI (`crabcc fetch`) and the live
//! dashboard's knowledge ingest endpoint (`POST /api/memory/ingest`).
//!
//! Three layers in increasing trust radius:
//!
//! 1. **Pure helpers** — `extract_urls`, `extract_title`, `url_host`,
//!    `extract_main_content`, `is_reddit_url`, `render_reddit_json`,
//!    `host_uses_article_extractor`. No I/O, no async; trivially
//!    testable. (Ported verbatim from `crates/crabcc-cli/src/fetch_cmd.rs`.)
//! 2. **`fetch_one`** — single-URL HTTP fetch with per-domain
//!    extractors (Reddit JSON API, article body for Medium/BBC/telex).
//!    Async, takes a pre-built `reqwest::Client`.
//! 3. **`fetch_and_clean`** — top-level "give me URLs, give me back
//!    cleaned markdown" entrypoint that the viz HTTP handler calls
//!    behind a tokio current-thread runtime.
//!
//! SSRF guards live in [`is_ingest_safe_url`] and run *before* the
//! request hits the wire — the viz layer enforces them; the CLI does
//! not (the CLI runs as the user, internal IPs may be desired).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::time::Duration;

const USER_AGENT: &str = concat!("crabcc-fetch/", env!("CARGO_PKG_VERSION"));

/// Default per-URL timeout for the CLI path.
pub const DEFAULT_PER_URL_TIMEOUT: Duration = Duration::from_secs(20);

/// Conservative per-URL timeout for the dashboard ingest endpoint —
/// half the CLI default so a misbehaving site can't pin a worker.
pub const INGEST_PER_URL_TIMEOUT: Duration = Duration::from_secs(15);

/// Hard cap on response body bytes accepted by the ingest endpoint.
/// 5 MiB picks up real article HTML (~ a few hundred KB pre-extract)
/// without letting a hostile server stream us into memory exhaustion.
pub const INGEST_MAX_BODY_BYTES: usize = 5 * 1024 * 1024;

/// At-most concurrent outgoing fetches the ingest endpoint will run.
/// Picked deliberately low because the viz process is single-user
/// localhost — even four parallel fetches is plenty.
pub const INGEST_CONCURRENCY: usize = 4;

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
    /// GitHub repo packed via repomix, surfaced by the CLI only.
    Repomix,
}

/// Extract HTTP/HTTPS URLs from arbitrary prose using `linkify`. Order
/// preserved; duplicates removed so the caller sees each URL exactly
/// once even if it's mentioned twice. Returns an empty Vec when no
/// URLs are present (deliberately not an error — callers may want to
/// fall back to "treat the whole input as freeform text").
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

/// Lowercase host for `http(s)://host[:port]/...`. Returns None for
/// malformed URLs without a `://` or with an empty host.
pub fn url_host(url: &str) -> Option<&str> {
    let after_scheme = url.split_once("://")?.1;
    let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    let host = &after_scheme[..host_end];
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// Domains where we prefer to extract `<article>` / `<main>` instead
/// of the full HTML — strips nav/footer/ads/sidebar noise. Unknown
/// hosts get the plain "convert the whole page" path (via `htmd`).
pub fn host_uses_article_extractor(host: &str) -> bool {
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

/// Find the first `<article>` element; fall back to the first `<main>`.
/// Returns None when neither is present so the caller can use the
/// whole HTML.
pub fn extract_main_content(html: &str) -> Option<&str> {
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
    let after_open = html[start..].find('>')? + start + 1;
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

pub fn is_reddit_url(url: &str) -> bool {
    matches!(
        url_host(url),
        Some("reddit.com" | "www.reddit.com" | "old.reddit.com" | "new.reddit.com")
    )
}

/// Pull `(title, markdown)` out of a Reddit `.json` response. Returns
/// None on shape surprise so the caller can surface a clear error.
pub fn render_reddit_json(body: &str) -> Option<(String, String)> {
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
        .unwrap_or_default()
        .trim();
    let post_url = post.get("url").and_then(|u| u.as_str()).unwrap_or_default();
    let score = post.get("score").and_then(|s| s.as_i64()).unwrap_or(0);

    let mut md = String::new();
    write!(
        md,
        "**r/{subreddit}**  \\| u/{author} \\| score {score}\n\n"
    )
    .unwrap();
    if !selftext.is_empty() {
        md.push_str(selftext);
        md.push_str("\n\n");
    } else if !post_url.is_empty() {
        write!(md, "link: {post_url}\n\n").unwrap();
    }

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
                .unwrap_or_default()
                .trim();
            if cbody.is_empty() {
                continue;
            }
            write!(md, "**u/{cauthor}** ({cscore}):\n{cbody}\n\n").unwrap();
        }
    }

    Some((title, md))
}

pub fn extract_title(html: &str) -> Option<String> {
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

// ───── SSRF guards (ingest-only) ────────────────────────────────────────

/// Reject URLs the dashboard's ingest endpoint must never try to fetch:
///   - non-http(s) schemes (`file:`, `ftp:`, `data:`, `gopher:`, …)
///   - hosts that resolve to localhost / RFC1918 / link-local / loopback
///   - explicit IP literals in those private ranges
///
/// Returns `Err(reason)` for rejected URLs and `Ok(())` for safe ones.
/// We do not perform DNS resolution here — that's intentionally
/// "best-effort string check"; the request layer will surface a
/// connect error for hosts that *resolve* to internal IPs even if they
/// don't look like one syntactically. Defence in depth, not perfect
/// containment.
pub fn is_ingest_safe_url(url: &str) -> Result<(), String> {
    let scheme_end = match url.find("://") {
        Some(i) => i,
        None => return Err("missing scheme".into()),
    };
    let scheme = &url[..scheme_end];
    if !matches!(scheme, "http" | "https") {
        return Err(format!("scheme `{scheme}` not allowed"));
    }
    let after = &url[scheme_end + 3..];
    let host_end = after.find('/').unwrap_or(after.len());
    let authority = &after[..host_end];
    // Strip user:pass@ — RFC says it's deprecated for HTTP but parsers accept it.
    let host_with_port = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    // Handle IPv6 literal `[::1]:port` form first — strip the brackets,
    // then optionally strip a `:port` suffix that came after `]`.
    let host = if let Some(stripped) = host_with_port.strip_prefix('[') {
        match stripped.split_once(']') {
            Some((inner, _rest)) => inner,
            None => return Err("malformed IPv6 host".into()),
        }
    } else {
        // Hostname or IPv4 — only strip a trailing `:port`.
        host_with_port
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_with_port)
    };
    if host.is_empty() {
        return Err("empty host".into());
    }
    let lower = host.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "localhost" | "0.0.0.0" | "broadcasthost" | "ip6-localhost" | "ip6-loopback" | "::1" | "::"
    ) || lower.ends_with(".localhost")
    {
        return Err("loopback host not allowed".into());
    }
    // IPv4 literal.
    if let Ok(addr) = lower.parse::<std::net::Ipv4Addr>() {
        if is_private_ipv4(&addr) {
            return Err(format!("private IPv4 `{addr}` not allowed"));
        }
    }
    if let Ok(addr) = lower.parse::<std::net::Ipv6Addr>() {
        if is_private_ipv6(&addr) {
            return Err(format!("private IPv6 `{addr}` not allowed"));
        }
    }
    Ok(())
}

fn is_private_ipv4(a: &std::net::Ipv4Addr) -> bool {
    a.is_loopback() || a.is_private() || a.is_link_local() || a.is_unspecified() || a.is_broadcast()
}

fn is_private_ipv6(a: &std::net::Ipv6Addr) -> bool {
    if a.is_loopback() || a.is_unspecified() {
        return true;
    }
    let seg = a.segments();
    // fc00::/7 (unique local) and fe80::/10 (link-local).
    (seg[0] & 0xfe00) == 0xfc00 || (seg[0] & 0xffc0) == 0xfe80
}

// ───── Single-URL fetch ─────────────────────────────────────────────────

/// Build a tuned reqwest client with the right user-agent + per-URL
/// timeout. Callers reuse the client across URLs for connection reuse.
pub fn make_client(per_url_timeout: Duration) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(per_url_timeout)
        .build()
}

/// Fetch one URL and clean it. Same per-domain branching as the CLI:
///   - Reddit hosts → `.json` API
///   - Medium/BBC/telex → `<article>`/`<main>` extraction
///   - everything else → whole-page HTML→Markdown (via `htmd`)
///
/// `max_body_bytes` is enforced when set: the body is read in 64 KiB
/// chunks and the request is dropped once the cap is exceeded. Pass
/// `None` for the CLI path (no cap).
pub async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    max_body_bytes: Option<usize>,
) -> FetchResult {
    if is_reddit_url(url) {
        return fetch_reddit_json(client, url, max_body_bytes).await;
    }
    let host = url_host(url).unwrap_or_default();
    let prefer_main = host_uses_article_extractor(host);
    match client.get(url).send().await {
        Err(e) => {
            tracing::warn!(target: "crabcc_fetch", url = %url, host = %host, error = %e, "fetch failed");
            FetchResult {
                url: url.into(),
                status: 0,
                title: None,
                content_markdown: None,
                via: Transport::Direct,
                error: Some(e.to_string()),
            }
        }
        Ok(resp) => {
            let status = resp.status().as_u16();
            match read_body_capped(resp, max_body_bytes).await {
                Err(e) => FetchResult {
                    url: url.into(),
                    status,
                    title: None,
                    content_markdown: None,
                    via: Transport::Direct,
                    error: Some(e),
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
                    // `panic = "abort"` release profile. `htmd::convert`
                    // returns `io::Result<String>`; on parse failure we
                    // fall back to an empty body. `.skip_tags(...)` drops
                    // style/script/noscript content (html2md silently
                    // dropped style; htmd defaults to serializing it as
                    // text) so the output stays close to the prior
                    // shape for the downstream consumers.
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

async fn fetch_reddit_json(
    client: &reqwest::Client,
    url: &str,
    max_body_bytes: Option<usize>,
) -> FetchResult {
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
            match read_body_capped(resp, max_body_bytes).await {
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

/// Read a response body with an optional byte cap. When the cap is
/// hit we drop the connection and return an error rather than
/// silently truncating — silent truncation produced corrupted HTML
/// that the HTML→Markdown converter couldn't parse cleanly.
async fn read_body_capped(
    resp: reqwest::Response,
    max_body_bytes: Option<usize>,
) -> std::result::Result<String, String> {
    let cap = match max_body_bytes {
        Some(n) => n,
        None => {
            return resp.text().await.map_err(|e| e.to_string());
        }
    };
    use bytes::BufMut;
    let mut buf = bytes::BytesMut::with_capacity(64 * 1024);
    let mut stream = resp;
    while let Some(chunk) = stream
        .chunk()
        .await
        .map_err(|e| format!("body chunk: {e}"))?
    {
        if buf.len() + chunk.len() > cap {
            tracing::warn!(target: "crabcc_fetch", cap, "response body exceeds cap; dropping connection");
            return Err(format!("response body exceeds {cap} bytes"));
        }
        buf.put_slice(&chunk);
    }
    String::from_utf8(buf.to_vec()).map_err(|e| format!("body utf-8: {e}"))
}

/// Top-level fetch+clean for a list of URLs. Spawns up to
/// `concurrency` fetches in parallel under an internal Semaphore.
/// Returns one FetchResult per input URL in submission order.
///
/// Callers wrap this in a tokio runtime — both the CLI's
/// `current_thread` runtime and the viz crate's worker thread can
/// use it.
pub async fn fetch_and_clean(urls: &[String], opts: FetchOpts) -> Vec<FetchResult> {
    if urls.is_empty() {
        return vec![];
    }
    tracing::debug!(
        target: "crabcc_fetch",
        url_count = urls.len(),
        concurrency = opts.concurrency,
        enforce_ssrf = opts.enforce_ssrf,
        max_body_bytes = ?opts.max_body_bytes,
        "fetch_and_clean start",
    );
    let client = match make_client(opts.per_url_timeout) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(target: "crabcc_fetch", error = %e, "reqwest client build failed");
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
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(opts.concurrency.max(1)));
    let max_body = opts.max_body_bytes;
    let enforce_ssrf = opts.enforce_ssrf;

    let mut set = tokio::task::JoinSet::new();
    for (idx, url) in urls.iter().cloned().enumerate() {
        let client = client.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.ok();
            if enforce_ssrf {
                if let Err(reason) = is_ingest_safe_url(&url) {
                    tracing::debug!(target: "crabcc_fetch", %url, %reason, "SSRF guard rejected url");
                    return (
                        idx,
                        FetchResult {
                            url,
                            status: 0,
                            title: None,
                            content_markdown: None,
                            via: Transport::Direct,
                            error: Some(reason),
                        },
                    );
                }
            }
            let res = fetch_one(&client, &url, max_body).await;
            (idx, res)
        });
    }

    let mut indexed: Vec<(usize, FetchResult)> = Vec::with_capacity(urls.len());
    while let Some(r) = set.join_next().await {
        match r {
            Ok(pair) => indexed.push(pair),
            Err(e) => indexed.push((
                indexed.len(),
                FetchResult {
                    url: String::new(),
                    status: 0,
                    title: None,
                    content_markdown: None,
                    via: Transport::Direct,
                    error: Some(format!("join error: {e}")),
                },
            )),
        }
    }
    indexed.sort_by_key(|(i, _)| *i);
    indexed.into_iter().map(|(_, r)| r).collect()
}

/// Tunables shared by both call sites. The viz layer flips
/// `enforce_ssrf` on; the CLI leaves it off so users can fetch their
/// own LAN servers.
#[derive(Debug, Clone, Copy)]
pub struct FetchOpts {
    pub per_url_timeout: Duration,
    pub concurrency: usize,
    pub max_body_bytes: Option<usize>,
    pub enforce_ssrf: bool,
}

impl FetchOpts {
    /// CLI defaults: 20s per URL, no body cap, no SSRF (user-driven).
    pub fn cli() -> Self {
        Self {
            per_url_timeout: DEFAULT_PER_URL_TIMEOUT,
            concurrency: 8,
            max_body_bytes: None,
            enforce_ssrf: false,
        }
    }

    /// Dashboard ingest defaults: shorter timeout, body cap, SSRF on,
    /// concurrency capped at 4.
    pub fn ingest() -> Self {
        Self {
            per_url_timeout: INGEST_PER_URL_TIMEOUT,
            concurrency: INGEST_CONCURRENCY,
            max_body_bytes: Some(INGEST_MAX_BODY_BYTES),
            enforce_ssrf: true,
        }
    }
}

// We pull `bytes` transitively via reqwest. Re-declare the minimal
// types we use as a pin so a reqwest version bump that drops the
// re-export breaks compilation here, not at a use-site downstream.
#[doc(hidden)]
pub use bytes;

// Re-export `linkify` so downstream crates (`crabcc-viz`) can build
// their own LinkFinder for URL-stripping passes without adding the
// crate to their own Cargo.toml.
pub use linkify;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_urls_dedup_and_order() {
        let p = "see https://a.example/x and http://b.example/y, also https://a.example/x";
        assert_eq!(
            extract_urls(p),
            vec!["https://a.example/x", "http://b.example/y"]
        );
    }

    #[test]
    fn extract_urls_ignores_non_http() {
        let p = "ftp://x file:///y data:text/plain,hi mailto:a@b";
        assert_eq!(extract_urls(p), Vec::<String>::new());
    }

    #[test]
    fn url_host_basic() {
        assert_eq!(url_host("https://example.com/x"), Some("example.com"));
        assert_eq!(url_host("http://a.b/"), Some("a.b"));
        assert_eq!(url_host("nogood"), None);
    }

    #[test]
    fn extract_title_basic() {
        let html = r#"<html><head><title>Hello</title></head></html>"#;
        assert_eq!(extract_title(html), Some("Hello".into()));
    }

    #[test]
    fn extract_title_with_attrs() {
        let html = r#"<title lang="en">Hi</title>"#;
        assert_eq!(extract_title(html), Some("Hi".into()));
    }

    #[test]
    fn extract_main_content_picks_article_first() {
        let html = "<body><nav>x</nav><article>body</article><main>main</main></body>";
        assert_eq!(extract_main_content(html), Some("body"));
    }

    #[test]
    fn ssrf_rejects_localhost_and_private() {
        for bad in [
            "http://localhost/x",
            "http://127.0.0.1/admin",
            "http://10.0.0.1/",
            "http://172.16.0.1/",
            "http://192.168.1.1/",
            "http://169.254.169.254/", // AWS IMDS link-local
            "http://[::1]/",
            "http://[fe80::1]/",
            "http://0.0.0.0/",
            "ftp://example.com/",
            "file:///etc/passwd",
            "data:text/plain,hi",
            "http://server.localhost/",
        ] {
            assert!(is_ingest_safe_url(bad).is_err(), "expected reject: {bad}");
        }
    }

    #[test]
    fn ssrf_accepts_public_urls() {
        for ok in [
            "http://example.com/",
            "https://example.com/path",
            "https://1.1.1.1/",
            "https://user:pass@example.com/",
        ] {
            assert!(is_ingest_safe_url(ok).is_ok(), "expected accept: {ok}");
        }
    }

    #[test]
    fn is_reddit_url_known_hosts() {
        assert!(is_reddit_url("https://www.reddit.com/r/x"));
        assert!(is_reddit_url("https://old.reddit.com/r/x"));
        assert!(!is_reddit_url("https://example.com/r/x"));
    }

    #[test]
    fn render_reddit_json_extracts_title_and_body() {
        let raw = r#"[
            {"data":{"children":[{"data":{
                "title":"Hello", "author":"u", "subreddit":"rust",
                "selftext":"Body here.", "score":42
            }}]}},
            {"data":{"children":[]}}
        ]"#;
        let (title, md) = render_reddit_json(raw).unwrap();
        assert_eq!(title, "Hello");
        assert!(md.contains("Body here."));
        assert!(md.contains("r/rust"));
    }

    #[test]
    fn fetch_opts_ingest_has_caps() {
        let o = FetchOpts::ingest();
        assert!(o.enforce_ssrf);
        assert!(o.max_body_bytes.is_some());
        assert!(o.concurrency <= INGEST_CONCURRENCY);
    }

    #[tokio::test]
    async fn fetch_and_clean_rejects_ssrf_with_clear_error() {
        let res = fetch_and_clean(&["http://127.0.0.1/admin".into()], FetchOpts::ingest()).await;
        assert_eq!(res.len(), 1);
        let r = &res[0];
        assert!(r.error.is_some());
        assert!(r.content_markdown.is_none());
    }

    #[tokio::test]
    async fn fetch_and_clean_handles_html_locally() {
        // Local oneshot HTTP server serving a tiny HTML payload — covers
        // the HTML→Markdown conversion path without leaving the test process.
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let body = "<html><head><title>T</title></head><body><p>hello world</p></body></html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\n\r\n{}",
                body.len(),
                body
            );
            s.write_all(resp.as_bytes()).unwrap();
        });
        let url = format!("http://127.0.0.1:{port}/");
        // Use CLI opts so SSRF doesn't reject the loopback request.
        let res = fetch_and_clean(&[url], FetchOpts::cli()).await;
        assert_eq!(res.len(), 1);
        let r = &res[0];
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert_eq!(r.title.as_deref(), Some("T"));
        assert!(r
            .content_markdown
            .as_deref()
            .unwrap_or_default()
            .contains("hello world"));
    }
}
