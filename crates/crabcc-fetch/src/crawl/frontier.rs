//! Crawl frontier + page store.
//!
//! Backend selection: a crawl prefers the shared **Hetzner Postgres**
//! queue (so multiple workers can split one frontier) but transparently
//! falls back to a **local SQLite file** when that server is unset or
//! unreachable. SQLite needs no external infrastructure — `rusqlite` is
//! bundled into the binary — so the fallback "just works" on any box.
//!
//! Only the SQLite path is implemented today; the Postgres backend is
//! the next layer and will share [`open_frontier`] so the fallback stays
//! invisible to the engine.

use crate::FetchResult;
use rusqlite::Connection;
use std::path::Path;

/// A URL waiting to be (or being) fetched, with its depth from the seed.
#[derive(Debug, Clone)]
pub struct Pending {
    pub url: String,
    pub depth: usize,
}

/// Local SQLite-backed frontier + page archive. The `frontier` table
/// doubles as the visited set (a URL present in any state is never
/// re-enqueued); `pages` is the durable result archive a crawl leaves
/// behind for later search/persistence.
pub struct SqliteFrontier {
    conn: Connection,
}

impl SqliteFrontier {
    /// Open (creating if needed) a frontier at `path`. Reopening the
    /// same path resumes an interrupted crawl: still-`queued` rows are
    /// picked back up, already-`done` URLs are skipped.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// In-memory frontier — for tests and throwaway one-shot crawls.
    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS frontier (
                 url           TEXT PRIMARY KEY,
                 depth         INTEGER NOT NULL,
                 state         TEXT NOT NULL DEFAULT 'queued',
                 host          TEXT,
                 discovered_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
             );
             CREATE INDEX IF NOT EXISTS frontier_state_depth ON frontier(state, depth);
             CREATE TABLE IF NOT EXISTS pages (
                 url              TEXT PRIMARY KEY,
                 status           INTEGER,
                 title            TEXT,
                 content_markdown TEXT,
                 error            TEXT,
                 depth            INTEGER,
                 fetched_at       INTEGER NOT NULL DEFAULT (strftime('%s','now'))
             );",
        )?;
        // A crash can leave rows stuck 'inflight'; on reopen treat them
        // as queued so a resumed run retries them.
        conn.execute(
            "UPDATE frontier SET state='queued' WHERE state='inflight'",
            [],
        )?;
        Ok(Self { conn })
    }

    /// Enqueue `url` at `depth` unless already known (visited-set dedup
    /// via the primary key). Returns `true` when newly inserted.
    pub fn enqueue(&self, url: &str, depth: usize, host: Option<&str>) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO frontier(url, depth, state, host) VALUES (?1, ?2, 'queued', ?3)",
            rusqlite::params![url, depth as i64, host],
        )?;
        Ok(n > 0)
    }

    /// Claim up to `limit` queued URLs (shallowest first), marking them
    /// `inflight` so a concurrent or resumed run won't redo them.
    ///
    /// Single atomic `UPDATE … RETURNING`: selecting then updating in two
    /// steps would let two workers sharing the same SQLite file observe
    /// the same queued rows before either commits. This claims and
    /// returns in one statement, so each row goes to exactly one worker.
    /// `RETURNING` doesn't preserve order, so re-sort by depth to keep
    /// the crawl breadth-first.
    pub fn claim(&self, limit: usize) -> anyhow::Result<Vec<Pending>> {
        let mut stmt = self.conn.prepare(
            "UPDATE frontier SET state='inflight'
             WHERE url IN (
                 SELECT url FROM frontier WHERE state='queued'
                 ORDER BY depth, rowid LIMIT ?1
             )
             RETURNING url, depth",
        )?;
        let mut rows: Vec<Pending> = stmt
            .query_map([limit as i64], |r| {
                Ok(Pending {
                    url: r.get(0)?,
                    depth: r.get::<_, i64>(1)? as usize,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows.sort_by_key(|p| p.depth);
        Ok(rows)
    }

    /// Transition a URL to a terminal state (`done` / `error`).
    pub fn mark(&self, url: &str, state: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE frontier SET state=?2 WHERE url=?1",
            rusqlite::params![url, state],
        )?;
        Ok(())
    }

    /// Persist a fetched page into the durable archive.
    pub fn record_page(&self, r: &FetchResult, depth: usize) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO pages(url,status,title,content_markdown,error,depth)
             VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![
                r.url,
                r.status as i64,
                r.title,
                r.content_markdown,
                r.error,
                depth as i64
            ],
        )?;
        Ok(())
    }

    /// `(archived_pages, still_queued)` — handy for progress reporting.
    pub fn counts(&self) -> anyhow::Result<(usize, usize)> {
        let pages: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))?;
        let queued: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM frontier WHERE state='queued'",
            [],
            |r| r.get(0),
        )?;
        Ok((pages as usize, queued as usize))
    }
}

/// Open the crawl frontier, preferring the Hetzner Postgres backend and
/// falling back to a local SQLite file at `local_path`.
///
/// `pg_url` is the shared-queue connection string (typically from
/// `$CRABCC_CRAWL_PG`). When it's `Some` but the Postgres backend isn't
/// available — not yet implemented today, unreachable tomorrow — we log
/// and drop to the local SQLite frontier so a crawl never hard-fails on
/// missing infrastructure.
pub fn open_frontier(local_path: &Path, pg_url: Option<&str>) -> anyhow::Result<SqliteFrontier> {
    if let Some(pg) = pg_url {
        // TODO(crawl-postgres): probe `pg`; on a successful connection
        // return the Postgres-backed frontier instead of falling through.
        // Log only the host — a connection string can embed
        // `user:password@`, which must never reach the logs.
        let redacted = url::Url::parse(pg)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| "<set>".to_string());
        tracing::warn!(
            target: "crabcc_fetch",
            pg_host = %redacted,
            "postgres crawl backend not wired yet; using local SQLite frontier"
        );
    }
    SqliteFrontier::open(local_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Transport;

    fn page(url: &str, status: u16) -> FetchResult {
        FetchResult {
            url: url.into(),
            status,
            title: None,
            content_markdown: Some("body".into()),
            via: Transport::Direct,
            error: None,
        }
    }

    #[test]
    fn enqueue_dedups_and_claim_orders_by_depth() {
        let f = SqliteFrontier::open_in_memory().unwrap();
        assert!(f.enqueue("https://s/a", 1, Some("s")).unwrap());
        assert!(f.enqueue("https://s/root", 0, Some("s")).unwrap());
        // Duplicate is ignored.
        assert!(!f.enqueue("https://s/a", 1, Some("s")).unwrap());

        let batch = f.claim(10).unwrap();
        assert_eq!(batch.len(), 2);
        // Shallowest first.
        assert_eq!(batch[0].url, "https://s/root");
        assert_eq!(batch[0].depth, 0);

        // Claimed rows are inflight, so a second claim sees nothing.
        assert!(f.claim(10).unwrap().is_empty());
    }

    #[test]
    fn record_and_count() {
        let f = SqliteFrontier::open_in_memory().unwrap();
        f.enqueue("https://s/x", 0, Some("s")).unwrap();
        let claimed = f.claim(10).unwrap();
        f.record_page(&page("https://s/x", 200), 0).unwrap();
        f.mark(&claimed[0].url, "done").unwrap();
        let (pages, queued) = f.counts().unwrap();
        assert_eq!(pages, 1);
        assert_eq!(queued, 0);
    }
}
