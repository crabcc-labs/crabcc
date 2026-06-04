//! Live smoke test for the crawl engine.
//!
//! ```text
//! cargo run -p crabcc-fetch --features crawl --example crawl_smoke -- \
//!     https://news.ycombinator.com/ 1 8
//! ```
//!
//! Args: `<seed-url> [max_depth=0] [max_pages=8]`. Uses the HTTP
//! transport (Lightpanda lands later) and an in-memory frontier; prints
//! one line per fetched page and a summary. Network-dependent, so it's an
//! example rather than a test — CI builds it but never runs it.

use std::sync::Arc;

use crabcc_fetch::crawl::{crawl, CrawlOpts, Fetcher, Frontier, SqliteFrontier};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let seed = args
        .next()
        .unwrap_or_else(|| "https://news.ycombinator.com/".to_string());
    let max_depth: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let max_pages: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);

    let opts = CrawlOpts::new(max_pages, max_depth);
    let frontier = Frontier::Sqlite(SqliteFrontier::open_in_memory()?);
    let fetcher = Arc::new(Fetcher::auto(&opts.fetch, None)?);

    println!(
        "crawling {seed}  (max_depth={max_depth}, max_pages={max_pages}, same_host={})",
        opts.same_host
    );
    let report = crawl(&seed, &opts, &frontier, fetcher, |r, depth| {
        let md_len = r.content_markdown.as_ref().map(|m| m.len()).unwrap_or(0);
        let title = r.title.as_deref().unwrap_or("");
        let status = match &r.error {
            Some(e) => format!("ERR {e}"),
            None => format!("{} ({md_len}B md)", r.status),
        };
        println!("  d{depth} {status:<24} {title:.60}  {}", r.url);
    })
    .await?;

    let (pages, queued) = frontier.counts().await.unwrap();
    println!(
        "\ndone: fetched={} errors={} discovered={} (archived={pages}, still_queued={queued})",
        report.fetched, report.errors, report.discovered
    );
    Ok(())
}
