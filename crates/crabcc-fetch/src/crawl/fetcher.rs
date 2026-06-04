//! Fetch transports for the crawler.
//!
//! The intended **default** transport is Lightpanda — a headless,
//! CDP-driven browser that returns the *rendered DOM* (so JS-built pages
//! work) without any screenshots or vision models: we pull `outerHTML`
//! over CDP and smart-parse it with the same pure helpers the
//! single-shot path uses ([`crate::clean_html`] + link extraction). That
//! transport lands in the next layer; until then — and whenever the
//! Lightpanda binary isn't present, e.g. in CI — we fall back to the
//! reqwest [`HttpFetcher`] (fast, but no JS execution).
//!
//! A transport returns both the cleaned [`FetchResult`] *and* the raw
//! HTML, because the crawler needs the raw markup to harvest links — the
//! single-shot `fetch_one` throws the HTML away after cleaning.

use crate::{
    clean_html, is_reddit_url, read_body_capped, url_host, FetchOpts, FetchResult, Transport,
    USER_AGENT,
};

/// A fetched page: the cleaned result plus the raw HTML the crawler uses
/// for link harvesting. `raw_html` is `None` for non-HTML bodies (e.g.
/// the Reddit JSON path), transport errors, or transports that don't
/// expose markup.
pub struct FetchedPage {
    pub result: FetchResult,
    pub raw_html: Option<String>,
}

/// reqwest-backed transport: fast, no JavaScript execution. The fallback
/// used when Lightpanda is unavailable and the default in tests.
pub struct HttpFetcher {
    client: reqwest::Client,
    max_body: Option<usize>,
}

impl HttpFetcher {
    /// Build an HTTP transport from the shared [`FetchOpts`]. When
    /// `proxy` is set (opt-in — see [`super::proxy`]) all requests route
    /// through it; treat every response as untrusted regardless.
    pub fn new(opts: &FetchOpts, proxy: Option<&str>) -> anyhow::Result<Self> {
        let mut builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(opts.per_url_timeout);
        if let Some(p) = proxy {
            builder = builder.proxy(reqwest::Proxy::all(p)?);
        }
        Ok(Self {
            client: builder.build()?,
            max_body: opts.max_body_bytes,
        })
    }

    async fn fetch(&self, url: &str) -> FetchedPage {
        // Reddit resolves to a JSON API with no crawlable HTML; defer to
        // the single-shot fetcher and surface no links from it.
        if is_reddit_url(url) {
            return FetchedPage {
                result: crate::fetch_one(&self.client, url, self.max_body).await,
                raw_html: None,
            };
        }
        let host = url_host(url).unwrap_or_default();
        match self.client.get(url).send().await {
            Err(e) => FetchedPage {
                result: error_result(url, e.to_string()),
                raw_html: None,
            },
            Ok(resp) => {
                let status = resp.status().as_u16();
                match read_body_capped(resp, self.max_body).await {
                    Err(e) => FetchedPage {
                        result: FetchResult {
                            url: url.into(),
                            status,
                            title: None,
                            content_markdown: None,
                            via: Transport::Direct,
                            error: Some(e),
                        },
                        raw_html: None,
                    },
                    Ok(html) => {
                        let (title, markdown) = clean_html(host, &html);
                        FetchedPage {
                            result: FetchResult {
                                url: url.into(),
                                status,
                                title,
                                content_markdown: Some(markdown),
                                via: Transport::Direct,
                                error: None,
                            },
                            raw_html: Some(html),
                        }
                    }
                }
            }
        }
    }
}

fn error_result(url: &str, error: String) -> FetchResult {
    FetchResult {
        url: url.into(),
        status: 0,
        title: None,
        content_markdown: None,
        via: Transport::Direct,
        error: Some(error),
    }
}

/// The active crawl transport. A closed enum (rather than `dyn`) so the
/// engine stays free of `async-trait`; Lightpanda joins as a variant.
pub enum Fetcher {
    Http(HttpFetcher),
    // Lightpanda(LightpandaFetcher),  // next layer: CDP rendered DOM.
}

impl Fetcher {
    /// Construct the reqwest transport. This is also the runtime fallback
    /// [`Fetcher::auto`] drops to when Lightpanda isn't available.
    pub fn http(opts: &FetchOpts, proxy: Option<&str>) -> anyhow::Result<Self> {
        Ok(Fetcher::Http(HttpFetcher::new(opts, proxy)?))
    }

    /// Select the default transport. Prefers Lightpanda (rendered DOM)
    /// and falls back to HTTP when its binary/endpoint isn't reachable.
    /// Until the Lightpanda transport lands this is HTTP unconditionally.
    pub fn auto(opts: &FetchOpts, proxy: Option<&str>) -> anyhow::Result<Self> {
        // TODO(crawl-lightpanda): if `lightpanda` is on PATH or
        // `$CRABCC_LIGHTPANDA_URL` is set, spawn/connect over CDP and
        // return `Fetcher::Lightpanda`; otherwise fall through.
        Self::http(opts, proxy)
    }

    pub async fn fetch(&self, url: &str) -> FetchedPage {
        match self {
            Fetcher::Http(f) => f.fetch(url).await,
        }
    }
}
