//! Fetch transports for the crawler.
//!
//! Intended fallback chain (each step tried when the previous can't
//! handle a URL — unavailable binary, JS-only page, or a bot wall):
//!
//! 1. **Lightpanda** (default) — headless, CDP-driven browser returning
//!    the *rendered DOM* (so JS-built pages work) with no screenshots or
//!    vision models: pull `outerHTML` over CDP and smart-parse it with
//!    the same pure helpers the single-shot path uses
//!    ([`crate::clean_html`] + link extraction).
//! 2. **[`HttpFetcher`]** (reqwest) — fast, no JS execution; the fallback
//!    when Lightpanda isn't present (e.g. CI) and the default in tests.
//! 3. **CloakBrowser** (ultimate fallback) — stealth Chromium (CDP /
//!    Playwright-compatible) for sites that block both of the above
//!    behind anti-bot fingerprinting. Same CDP seam as Lightpanda.
//!
//! Only steps 1 and 3 are the next layer; step 2 ships here.
//!
//! **Output is always token-friendly markdown.** A transport returns the
//! cleaned [`FetchResult`] (whose `content_markdown` is htmd-converted
//! markdown, never raw HTML) *and* the raw HTML — the latter only so the
//! crawler can harvest links; `fetch_one` throws the HTML away after
//! cleaning. A compact `msgpack` serialization of `FetchResult` for
//! shipping batches to an LLM is a planned opt-in.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use super::proxy::ProxyPool;
use crate::{
    clean_html, is_reddit_url, read_body_capped, url_host, FetchOpts, FetchResult, Transport,
    USER_AGENT,
};

/// A fetched page: the cleaned result plus the raw HTML the crawler uses
/// for link harvesting. `raw_html` is `None` for non-HTML bodies (e.g.
/// the Reddit JSON path), transport errors, or transports that don't
/// expose markup.
///
/// `final_url` is the URL *after* any redirects — the body came from
/// there, so relative links must resolve against it and host-scoping
/// must compare against it, not the originally-requested URL. Falls back
/// to the request URL when no redirect occurred or on error.
pub struct FetchedPage {
    pub result: FetchResult,
    pub raw_html: Option<String>,
    pub final_url: String,
}

/// reqwest-backed transport: fast, no JavaScript execution. The fallback
/// used when Lightpanda is unavailable and the default in tests.
///
/// With a [`ProxyPool`] attached ([`HttpFetcher::with_pool`]) every
/// request is routed through a proxy the pool picks, and the pool is told
/// whether that proxy delivered a response — so dead proxies are evicted
/// mid-crawl (see [`super::proxy`]). A reqwest client is cached per proxy
/// (clients clone cheaply but aren't cheap to build).
pub struct HttpFetcher {
    /// No-proxy base client: used directly when no pool is attached, and
    /// as the fallback while the pool is momentarily empty.
    client: reqwest::Client,
    max_body: Option<usize>,
    timeout: Duration,
    pool: Option<Arc<ProxyPool>>,
    proxy_clients: RwLock<HashMap<String, reqwest::Client>>,
}

impl HttpFetcher {
    /// Build an HTTP transport from the shared [`FetchOpts`]. When `proxy`
    /// is set (a single fixed proxy) all requests route through it; treat
    /// every response as untrusted regardless.
    pub fn new(opts: &FetchOpts, proxy: Option<&str>) -> anyhow::Result<Self> {
        Ok(Self {
            client: build_client(opts.per_url_timeout, proxy)?,
            max_body: opts.max_body_bytes,
            timeout: opts.per_url_timeout,
            pool: None,
            proxy_clients: RwLock::new(HashMap::new()),
        })
    }

    /// Build an HTTP transport that rotates through `pool`, evicting dead
    /// proxies as it goes. Requests fall back to a direct connection while
    /// the pool is empty.
    pub fn with_pool(opts: &FetchOpts, pool: Arc<ProxyPool>) -> anyhow::Result<Self> {
        Ok(Self {
            client: build_client(opts.per_url_timeout, None)?,
            max_body: opts.max_body_bytes,
            timeout: opts.per_url_timeout,
            pool: Some(pool),
            proxy_clients: RwLock::new(HashMap::new()),
        })
    }

    /// Choose the client for the next request: a pooled proxy when one is
    /// available, else the direct base client. Returns the chosen proxy
    /// (if any) so the caller can report its health back to the pool.
    fn select(&self) -> (reqwest::Client, Option<String>) {
        if let Some(pool) = &self.pool {
            if let Some(proxy) = pool.pick() {
                return (self.client_for(&proxy), Some(proxy));
            }
        }
        (self.client.clone(), None)
    }

    /// Get-or-build (and cache) the client routed through `proxy`. Falls
    /// back to the direct client if the proxy URL won't build.
    fn client_for(&self, proxy: &str) -> reqwest::Client {
        if let Some(c) = self.proxy_clients.read().unwrap().get(proxy) {
            return c.clone();
        }
        match build_client(self.timeout, Some(proxy)) {
            Ok(c) => {
                self.proxy_clients
                    .write()
                    .unwrap()
                    .insert(proxy.to_string(), c.clone());
                c
            }
            Err(_) => self.client.clone(),
        }
    }

    /// Feed a proxy's outcome back to the pool. `ok` = it delivered a
    /// response (even an HTTP error status); `false` = a transport-level
    /// failure (connect/timeout) we attribute to the proxy. Drop the
    /// cached client when a failure evicts the proxy.
    fn report(&self, proxy: &str, ok: bool) {
        if let Some(pool) = &self.pool {
            if ok {
                pool.report_success(proxy);
            } else if pool.report_failure(proxy) {
                self.proxy_clients.write().unwrap().remove(proxy);
            }
        }
    }

    pub(crate) async fn fetch(&self, url: &str) -> FetchedPage {
        // Reddit resolves to a JSON API with no crawlable HTML; defer to
        // the single-shot fetcher (direct client) and surface no links.
        if is_reddit_url(url) {
            return FetchedPage {
                result: crate::fetch_one(&self.client, url, self.max_body).await,
                raw_html: None,
                final_url: url.into(),
            };
        }
        let (client, proxy) = self.select();
        match client.get(url).send().await {
            Err(e) => {
                if let Some(p) = &proxy {
                    self.report(p, false);
                }
                FetchedPage {
                    result: error_result(url, e.to_string()),
                    raw_html: None,
                    final_url: url.into(),
                }
            }
            Ok(resp) => {
                if let Some(p) = &proxy {
                    self.report(p, true);
                }
                let status = resp.status().as_u16();
                // The effective URL after redirects — the body is from
                // here, so links resolve and scope against it.
                let final_url = resp.url().to_string();
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
                        final_url,
                    },
                    Ok(html) => {
                        let host = url_host(&final_url).unwrap_or_default();
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
                            final_url,
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

/// Build a reqwest client, optionally routed through `proxy`.
fn build_client(timeout: Duration, proxy: Option<&str>) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout);
    if let Some(p) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(p)?);
    }
    Ok(builder.build()?)
}

/// The active crawl transport. A closed enum (rather than `dyn`) so the
/// engine stays free of `async-trait`; Lightpanda joins as a variant.
pub enum Fetcher {
    Http(HttpFetcher),
    #[cfg(feature = "crawl-lightpanda")]
    Lightpanda(super::lightpanda::LightpandaFetcher),
}

impl Fetcher {
    /// Construct the reqwest transport. This is also the runtime fallback
    /// [`Fetcher::auto`] drops to when Lightpanda isn't available.
    pub fn http(opts: &FetchOpts, proxy: Option<&str>) -> anyhow::Result<Self> {
        Ok(Fetcher::Http(HttpFetcher::new(opts, proxy)?))
    }

    /// Construct the HTTP transport routed through a rotating, self-healing
    /// [`ProxyPool`]. Used by `crabcc crawl --proxify`.
    pub fn http_with_pool(opts: &FetchOpts, pool: Arc<ProxyPool>) -> anyhow::Result<Self> {
        Ok(Fetcher::Http(HttpFetcher::with_pool(opts, pool)?))
    }

    /// Select the default transport. Prefers Lightpanda (rendered DOM,
    /// behind `crawl-lightpanda`) and falls back to the native HTTP
    /// transport when its binary/endpoint isn't reachable. The per-page
    /// fetch *also* falls back to HTTP on any browser failure, so a flaky
    /// browser never drops a page.
    pub fn auto(opts: &FetchOpts, proxy: Option<&str>) -> anyhow::Result<Self> {
        #[cfg(feature = "crawl-lightpanda")]
        if let Some(cfg) = super::lightpanda::LightpandaConfig::from_env() {
            match super::lightpanda::LightpandaFetcher::start(cfg, opts, proxy) {
                Ok(f) => {
                    tracing::info!(
                        target: "crabcc_fetch",
                        endpoint = %f.ws_url(),
                        "lightpanda transport active",
                    );
                    return Ok(Fetcher::Lightpanda(f));
                }
                Err(e) => tracing::warn!(
                    target: "crabcc_fetch",
                    error = %e,
                    "lightpanda unavailable; falling back to HTTP transport",
                ),
            }
        }
        Self::http(opts, proxy)
    }

    pub async fn fetch(&self, url: &str) -> FetchedPage {
        match self {
            Fetcher::Http(f) => f.fetch(url).await,
            #[cfg(feature = "crawl-lightpanda")]
            Fetcher::Lightpanda(f) => f.fetch(url).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_pool_select_uses_the_direct_client() {
        let f = HttpFetcher::new(&FetchOpts::cli(), None).unwrap();
        let (_client, proxy) = f.select();
        assert!(proxy.is_none());
    }

    #[test]
    fn reported_failures_evict_the_proxy_through_the_pool() {
        let pool = Arc::new(ProxyPool::from_list(vec!["http://p:1".into()]).with_max_failures(2));
        let f = HttpFetcher::with_pool(&FetchOpts::cli(), pool.clone()).unwrap();
        f.report("http://p:1", false);
        assert_eq!(pool.len(), 1); // one strike, still in rotation
        f.report("http://p:1", false);
        assert_eq!(pool.len(), 0); // second strike → evicted
    }

    #[test]
    fn a_success_clears_a_proxys_failure_streak() {
        let pool = Arc::new(ProxyPool::from_list(vec!["http://p:1".into()]).with_max_failures(2));
        let f = HttpFetcher::with_pool(&FetchOpts::cli(), pool.clone()).unwrap();
        f.report("http://p:1", false);
        f.report("http://p:1", true); // reset
        f.report("http://p:1", false);
        assert_eq!(pool.len(), 1); // never reached two consecutive
    }
}
