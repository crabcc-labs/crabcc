//! `crabcc crawl <url>` — multi-page crawl built on `crabcc-fetch`'s
//! crawl engine. Streams each fetched page (markdown) into the memory
//! Palace as it lands (with `--remember`) so a long crawl is searchable
//! before it finishes, and prints a JSON array (or streamed text) plus a
//! summary on stderr.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use crabcc_fetch::crawl::{
    crawl, open_frontier, CrawlOpts, Fetcher, Frontier, Protocol, ProxiflySource, ProxyPool,
    SqliteFrontier,
};
use crabcc_fetch::{url_host, FetchResult};
use crabcc_memory::Palace;

/// Run a crawl from `seed`.
///
/// - `depth`: hops from the seed (`0` = seed only).
/// - `max_pages`: hard cap on pages fetched.
/// - `all_hosts`: follow off-host links too (default: stay on seed host).
/// - `remember`: persist each successful page into `<root>` memory under
///   wing=`crawl`, room=host, source_id=url.
/// - `format`: `json` (default) or `text`.
/// - `proxify`: when set, route fetches through a rotating proxifly pool
///   of that protocol (`http`/`https`/`socks4`/`socks5`).
/// - `state`: resume from a persistent on-disk frontier at this path
///   (created if absent); default is an ephemeral in-memory frontier.
#[allow(clippy::too_many_arguments)]
pub fn run(
    root: &Path,
    seed: &str,
    depth: usize,
    max_pages: usize,
    all_hosts: bool,
    concurrency: usize,
    remember: bool,
    format: &str,
    proxify: Option<&str>,
    state: Option<&Path>,
) -> Result<()> {
    if url_host(seed).is_none() {
        anyhow::bail!("crawl: `{seed}` is not an http(s) URL");
    }

    let mut opts = CrawlOpts::new(max_pages, depth);
    opts.same_host = !all_hosts;
    if concurrency > 0 {
        opts.concurrency = concurrency;
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    // Frontier backend, in precedence order:
    //   --state PATH        → resumable on-disk SQLite frontier (reopening
    //                         the same path picks up still-queued rows and
    //                         skips done URLs).
    //   $CRABCC_CRAWL_PG    → shared Postgres queue (open_frontier falls
    //                         back to local SQLite if it's unreachable or
    //                         the crawl-postgres feature isn't built).
    //   (neither)           → ephemeral in-memory SQLite frontier.
    // The durable output is the Palace archive (with --remember) regardless.
    let frontier = if let Some(state_path) = state {
        if let Some(parent) = state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Frontier::Sqlite(SqliteFrontier::open(state_path)?)
    } else if let Some(pg) = std::env::var("CRABCC_CRAWL_PG")
        .ok()
        .filter(|v| !v.is_empty())
    {
        let fallback = root.join(".crabcc").join("crawl-frontier.db");
        if let Some(parent) = fallback.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        rt.block_on(open_frontier(&fallback, Some(&pg)))?
    } else {
        Frontier::Sqlite(SqliteFrontier::open_in_memory()?)
    };
    let fetcher = match proxify {
        Some(proto) => {
            let protocol: Protocol = proto.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let loader = reqwest::Client::builder().build()?;
            let source = ProxiflySource::new(protocol);
            let pool = rt.block_on(ProxyPool::load(&loader, &source))?;
            if pool.is_empty() {
                anyhow::bail!("crawl --proxify {proto}: proxy source returned no proxies");
            }
            eprintln!("[crawl] proxify: {} {proto} proxies loaded", pool.len());
            Arc::new(Fetcher::http_with_pool(&opts.fetch, Arc::new(pool))?)
        }
        None => Arc::new(Fetcher::auto(&opts.fetch, None)?),
    };

    let palace = if remember {
        Some(Palace::open(root)?)
    } else {
        None
    };
    let session = std::env::var("TERM_SESSION_ID").ok();
    let text = format == "text";
    let mut collected: Vec<FetchResult> = Vec::new();

    let report = rt.block_on(async {
        crawl(seed, &opts, &frontier, fetcher, |r, d| {
            if let Some(p) = &palace {
                persist(p, r, session.as_deref());
            }
            if text {
                print_page(r, d);
            } else {
                collected.push(r.clone());
            }
        })
        .await
    })?;

    if !text {
        println!("{}", serde_json::to_string_pretty(&collected)?);
    }
    eprintln!(
        "crawl: fetched={} errors={} discovered={}{}",
        report.fetched,
        report.errors,
        report.discovered,
        if remember {
            " (saved to memory wing=crawl)"
        } else {
            ""
        }
    );
    Ok(())
}

/// Persist one successful page into the Palace (best-effort, mirrors
/// `fetch --remember`). Errors and bodyless pages are skipped.
fn persist(palace: &Palace, r: &FetchResult, session: Option<&str>) {
    if r.error.is_some() {
        return;
    }
    let Some(md) = &r.content_markdown else {
        return;
    };
    let host = url_host(&r.url).unwrap_or("unknown");
    let title = r.title.as_deref().unwrap_or_default();
    let body = if title.is_empty() {
        md.clone()
    } else {
        format!("# {title}\n\n{md}")
    };
    let _ = palace.remember_in_session("crawl", Some(host), &r.url, &body, session);
}

fn print_page(r: &FetchResult, depth: usize) {
    match &r.error {
        Some(e) => println!("d{depth} ERR  {}  {e}", r.url),
        None => {
            let md_len = r.content_markdown.as_ref().map(|m| m.len()).unwrap_or(0);
            let title = r.title.as_deref().unwrap_or("");
            println!(
                "d{depth} {:<3} {md_len:>7}B  {title:.60}  {}",
                r.status, r.url
            );
        }
    }
}
