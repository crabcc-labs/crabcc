//! Multi-page crawler built on the single-shot fetch primitives.
//!
//! The engine ([`crawl`]) drives a [`SqliteFrontier`] (URL frontier +
//! visited set + page archive) through a [`Fetcher`] transport, harvests
//! links from each fetched page, and re-enqueues in-scope targets until a
//! `max_pages` / `max_depth` budget is hit. Concurrency is bounded both
//! globally and per-host (politeness), so one host can't be hammered.
//!
//! Layers:
//! - [`links`] — pure `<a href>` extraction + resolution.
//! - [`frontier`] — SQLite frontier (Postgres fallback target).
//! - [`fetcher`] — transports (HTTP today, Lightpanda next).
//! - [`proxy`] — opt-in rotating proxy pool.

#[cfg(feature = "crawl-lightpanda")]
mod cdp;
mod fetcher;
mod frontier;
#[cfg(feature = "crawl-lightpanda")]
pub mod lightpanda;
mod links;
mod proxy;

pub use fetcher::{FetchedPage, Fetcher, HttpFetcher};
pub use frontier::{open_frontier, Pending, SqliteFrontier};
pub use links::extract_links;
pub use proxy::{Protocol, ProxiflySource, ProxyPool, ProxySource, ProxyUrl};

use crate::{is_ingest_safe_url, url_host, FetchOpts, FetchResult};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Crawl tunables. Construct via [`CrawlOpts::new`] and tweak fields.
#[derive(Debug, Clone)]
pub struct CrawlOpts {
    /// Hard cap on pages fetched (including errors). Primary stop signal.
    pub max_pages: usize,
    /// Maximum hops from the seed. `0` fetches only the seed; `1` fetches
    /// the seed plus everything it links to, etc. Gates link-following,
    /// not whether an already-queued page is fetched.
    pub max_depth: usize,
    /// Restrict the frontier to links sharing the seed's host.
    pub same_host: bool,
    /// Max concurrent in-flight fetches across the whole crawl.
    pub concurrency: usize,
    /// Max concurrent fetches to any single host (politeness).
    pub per_host_concurrency: usize,
    /// Per-URL fetch tunables (timeout, body cap, SSRF) — reused from the
    /// single-shot path. SSRF enforcement here also filters discovered
    /// links before they're enqueued.
    pub fetch: FetchOpts,
}

/// Default per-page response body cap for crawls. `FetchOpts::cli()`
/// leaves the body uncapped — fine for a single user-driven fetch, but a
/// crawl pulls many pages from untrusted hosts, so a hostile/huge response
/// must not be able to stream the process into OOM. Generous enough for
/// real article HTML; override `opts.fetch.max_body_bytes` to change it.
pub const CRAWL_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

impl CrawlOpts {
    /// Defaults: same-host, polite (1 request per host at a time), 4-way
    /// global concurrency, CLI fetch posture (SSRF off) but with a
    /// [`CRAWL_MAX_BODY_BYTES`] body cap applied (crawls are untrusted).
    pub fn new(max_pages: usize, max_depth: usize) -> Self {
        let mut fetch = FetchOpts::cli();
        fetch.max_body_bytes = Some(CRAWL_MAX_BODY_BYTES);
        Self {
            max_pages,
            max_depth,
            same_host: true,
            concurrency: 4,
            per_host_concurrency: 1,
            fetch,
        }
    }
}

/// Summary of a crawl run.
#[derive(Debug, Default, Clone, Copy)]
pub struct CrawlReport {
    /// Pages fetched (successes + errors).
    pub fetched: usize,
    /// Of those, how many returned a transport/HTTP error.
    pub errors: usize,
    /// Newly discovered URLs enqueued into the frontier.
    pub discovered: usize,
}

const SHUTDOWN_REASON: &str = "crawl shutting down (semaphore closed)";

/// Build the `(url, depth, page)` tuple the dispatch loop expects for a
/// URL rejected before any fetch — the SSRF guard, or a torn-down crawl.
fn rejected(url: String, depth: usize, reason: String) -> (String, usize, FetchedPage) {
    let result = FetchResult {
        url: url.clone(),
        status: 0,
        title: None,
        content_markdown: None,
        via: crate::Transport::Direct,
        error: Some(reason),
    };
    (
        url.clone(),
        depth,
        FetchedPage {
            result,
            raw_html: None,
            final_url: url,
        },
    )
}

/// Crawl from `seed`, persisting each page into `frontier` and invoking
/// `on_page(result, depth)` for every fetched page (the hook the CLI uses
/// to mirror results into Palace). Returns once the frontier drains or
/// the `max_pages` budget is spent.
///
/// `fetcher` is `Arc`-shared so per-URL fetches can run on the runtime's
/// worker threads; the frontier and `on_page` hook stay on the calling
/// task (SQLite's connection isn't shared across threads).
pub async fn crawl(
    seed: &str,
    opts: &CrawlOpts,
    frontier: &SqliteFrontier,
    fetcher: Arc<Fetcher>,
    mut on_page: impl FnMut(&FetchResult, usize),
) -> anyhow::Result<CrawlReport> {
    // Updated to the seed's *effective* host once fetched, so a seed
    // that redirects (example.com → www.example.com) scopes to where it
    // landed rather than dropping every link as off-host.
    let mut seed_host = url_host(seed).unwrap_or_default().to_string();
    frontier.enqueue(seed, 0, Some(&seed_host))?;

    let mut report = CrawlReport::default();
    let global = Arc::new(Semaphore::new(opts.concurrency.max(1)));
    let mut host_sems: HashMap<String, Arc<Semaphore>> = HashMap::new();

    loop {
        if report.fetched >= opts.max_pages {
            break;
        }
        let budget = opts.max_pages - report.fetched;
        let batch = frontier.claim(budget.min(opts.concurrency))?;
        if batch.is_empty() {
            break;
        }

        // Dispatch the batch concurrently, bounded globally and per-host.
        let mut set = tokio::task::JoinSet::new();
        for p in batch {
            let host = url_host(&p.url).unwrap_or_default().to_string();
            let hsem = host_sems
                .entry(host)
                .or_insert_with(|| Arc::new(Semaphore::new(opts.per_host_concurrency.max(1))))
                .clone();
            let gsem = global.clone();
            let f = fetcher.clone();
            let enforce_ssrf = opts.fetch.enforce_ssrf;
            let (url, depth) = (p.url, p.depth);
            set.spawn(async move {
                // SSRF guard at fetch time — mirrors `fetch_and_clean`,
                // which checks before every `fetch_one`. The enqueue-time
                // filter below only covers *discovered* links; the seed
                // and any resumed/queued rows must be checked here too, or
                // an ingest-posture crawl could hit `169.254.169.254` etc.
                if enforce_ssrf {
                    if let Err(reason) = is_ingest_safe_url(&url) {
                        return rejected(url, depth, reason);
                    }
                }
                // Hold both permits (global + per-host politeness) for the
                // fetch. A closed semaphore means the crawl is tearing
                // down — bail out instead of fetching unthrottled, which is
                // what `.ok()` (discarding the permit) would have done.
                let _g = match gsem.acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => return rejected(url, depth, SHUTDOWN_REASON.into()),
                };
                let _h = match hsem.acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => return rejected(url, depth, SHUTDOWN_REASON.into()),
                };
                let page = f.fetch(&url).await;
                (url, depth, page)
            });
        }

        // Drain results on this task: record, report, harvest links.
        while let Some(joined) = set.join_next().await {
            let (url, depth, page) = match joined {
                Ok(t) => t,
                Err(_) => continue, // join error (panic/cancel): skip
            };
            frontier.record_page(&page.result, depth)?;
            if page.result.error.is_some() {
                report.errors += 1;
                frontier.mark(&url, "error")?;
            } else {
                frontier.mark(&url, "done")?;
            }
            report.fetched += 1;
            on_page(&page.result, depth);

            // The seed is the only depth-0 page; adopt its post-redirect
            // host as the same-host scope.
            if depth == 0 {
                if let Some(h) = url_host(&page.final_url) {
                    seed_host = h.to_string();
                }
            }

            if depth < opts.max_depth {
                if let Some(html) = &page.raw_html {
                    // Resolve against the effective (post-redirect) URL.
                    for link in extract_links(&page.final_url, html) {
                        if opts.same_host && url_host(&link).unwrap_or_default() != seed_host {
                            continue;
                        }
                        if opts.fetch.enforce_ssrf && is_ingest_safe_url(&link).is_err() {
                            continue;
                        }
                        let lh = url_host(&link).unwrap_or_default().to_string();
                        if frontier.enqueue(&link, depth + 1, Some(&lh))? {
                            report.discovered += 1;
                        }
                    }
                }
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Minimal blocking HTTP/1.1 server returning canned HTML per path.
    /// `std::net` (not tokio) so the test needs no extra tokio io feature.
    fn spawn_server(routes: Vec<(&'static str, &'static str)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                let body = routes
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, b)| *b)
                    .unwrap_or("");
                // A body of `302:/dest` emits a redirect to /dest.
                let resp = if let Some(loc) = body.strip_prefix("302:") {
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                };
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn crawls_same_host_respecting_depth_and_scope() {
        let base = spawn_server(vec![
            (
                "/",
                r#"<a href="/a">a</a><a href="/b">b</a><a href="http://external.invalid/x">ext</a>"#,
            ),
            ("/a", r#"<a href="/c">c</a>"#),
            ("/b", "leaf b"),
            ("/c", "leaf c"),
        ]);
        let seed = format!("{base}/");

        let frontier = SqliteFrontier::open_in_memory().unwrap();
        let opts = CrawlOpts::new(50, 1); // seed + one hop
        let fetcher = Arc::new(Fetcher::http(&opts.fetch, None).unwrap());

        let mut seen = Vec::new();
        let report = crawl(&seed, &opts, &frontier, fetcher, |r, _d| {
            seen.push(r.url.clone())
        })
        .await
        .unwrap();

        // Seed + /a + /b. /c is depth 2 (beyond max_depth=1); external
        // host is filtered by same_host.
        assert_eq!(report.fetched, 3, "fetched: {:?}", seen);
        assert_eq!(report.errors, 0);
        assert!(seen.iter().any(|u| u.ends_with("/a")));
        assert!(seen.iter().any(|u| u.ends_with("/b")));
        assert!(!seen.iter().any(|u| u.ends_with("/c")));
        assert!(!seen.iter().any(|u| u.contains("external.invalid")));

        let (pages, _queued) = frontier.counts().unwrap();
        assert_eq!(pages, 3);
    }

    #[tokio::test]
    async fn max_pages_caps_the_crawl() {
        let base = spawn_server(vec![
            (
                "/",
                r#"<a href="/a">a</a><a href="/b">b</a><a href="/c">c</a>"#,
            ),
            ("/a", "a"),
            ("/b", "b"),
            ("/c", "c"),
        ]);
        let seed = format!("{base}/");
        let frontier = SqliteFrontier::open_in_memory().unwrap();
        let opts = CrawlOpts::new(2, 5); // cap at 2 pages
        let fetcher = Arc::new(Fetcher::http(&opts.fetch, None).unwrap());
        let report = crawl(&seed, &opts, &frontier, fetcher, |_, _| {})
            .await
            .unwrap();
        assert_eq!(report.fetched, 2);
    }

    #[test]
    fn crawl_defaults_cap_response_body() {
        // Crawls are untrusted: a body cap must be set even though the
        // underlying cli() posture leaves it open.
        assert_eq!(
            CrawlOpts::new(1, 0).fetch.max_body_bytes,
            Some(CRAWL_MAX_BODY_BYTES)
        );
    }

    #[tokio::test]
    async fn resolves_links_against_redirected_url() {
        // /old → /new/, whose body links to a *relative* `page`. The link
        // must resolve against /new/ (→ /new/page), not the seed /old.
        let base = spawn_server(vec![
            ("/old", "302:/new/"),
            ("/new/", r#"<a href="page">deeper</a>"#),
            ("/new/page", "leaf"),
        ]);
        let seed = format!("{base}/old");

        let frontier = SqliteFrontier::open_in_memory().unwrap();
        let opts = CrawlOpts::new(50, 1);
        let fetcher = Arc::new(Fetcher::http(&opts.fetch, None).unwrap());

        let mut seen = Vec::new();
        crawl(&seed, &opts, &frontier, fetcher, |r, _| {
            seen.push(r.url.clone())
        })
        .await
        .unwrap();

        // The relative link resolved against /new/, so /new/page was
        // crawled; a naive resolve against /old would have produced
        // /page (404, never linked) instead.
        assert!(
            seen.iter().any(|u| u.ends_with("/new/page")),
            "expected /new/page to be discovered, saw: {seen:?}"
        );
    }

    #[tokio::test]
    async fn ssrf_guard_blocks_private_seed_in_ingest_posture() {
        // No server: the guard must reject the seed *before* any request,
        // so a private/metadata address can't be hit even as the seed.
        let frontier = SqliteFrontier::open_in_memory().unwrap();
        let mut opts = CrawlOpts::new(5, 1);
        opts.fetch = crate::FetchOpts::ingest(); // enforce_ssrf = true
        let fetcher = Arc::new(Fetcher::http(&opts.fetch, None).unwrap());

        let mut errors = Vec::new();
        let report = crawl(
            "http://169.254.169.254/latest/meta-data/",
            &opts,
            &frontier,
            fetcher,
            |r, _| {
                if let Some(e) = &r.error {
                    errors.push(e.clone());
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(report.fetched, 1);
        assert_eq!(report.errors, 1);
        assert_eq!(report.discovered, 0);
        assert!(
            errors[0].contains("not allowed"),
            "expected SSRF rejection, got: {errors:?}"
        );
    }
}
