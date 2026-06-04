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

/// A crawl frontier backend: local SQLite (default, zero-infra) or a
/// shared Postgres queue (multi-worker, behind `crawl-postgres`). The
/// engine drives this; [`open_frontier`] chooses the backend.
///
/// Methods are `async` so the Postgres backend can await its client; the
/// SQLite arm runs its synchronous rusqlite calls inline. A closed enum
/// (rather than `dyn`) keeps the engine free of `async-trait` and dynamic
/// dispatch.
pub enum Frontier {
    Sqlite(SqliteFrontier),
    #[cfg(feature = "crawl-postgres")]
    Postgres(PostgresFrontier),
}

// `unused_async` fires on the SQLite-only build (no `.await` in any arm);
// the `async` is load-bearing once the Postgres backend is compiled in.
#[allow(clippy::unused_async)]
impl Frontier {
    pub async fn enqueue(
        &self,
        url: &str,
        depth: usize,
        host: Option<&str>,
    ) -> anyhow::Result<bool> {
        match self {
            Frontier::Sqlite(f) => f.enqueue(url, depth, host),
            #[cfg(feature = "crawl-postgres")]
            Frontier::Postgres(f) => f.enqueue(url, depth, host).await,
        }
    }

    pub async fn claim(&self, limit: usize) -> anyhow::Result<Vec<Pending>> {
        match self {
            Frontier::Sqlite(f) => f.claim(limit),
            #[cfg(feature = "crawl-postgres")]
            Frontier::Postgres(f) => f.claim(limit).await,
        }
    }

    pub async fn mark(&self, url: &str, state: &str) -> anyhow::Result<()> {
        match self {
            Frontier::Sqlite(f) => f.mark(url, state),
            #[cfg(feature = "crawl-postgres")]
            Frontier::Postgres(f) => f.mark(url, state).await,
        }
    }

    pub async fn record_page(&self, r: &FetchResult, depth: usize) -> anyhow::Result<()> {
        match self {
            Frontier::Sqlite(f) => f.record_page(r, depth),
            #[cfg(feature = "crawl-postgres")]
            Frontier::Postgres(f) => f.record_page(r, depth).await,
        }
    }

    pub async fn counts(&self) -> anyhow::Result<(usize, usize)> {
        match self {
            Frontier::Sqlite(f) => f.counts(),
            #[cfg(feature = "crawl-postgres")]
            Frontier::Postgres(f) => f.counts().await,
        }
    }
}

/// Shared Postgres-backed frontier — a single queue multiple crawl workers
/// can split. `claim` uses `FOR UPDATE SKIP LOCKED` so each queued row goes
/// to exactly one worker without blocking the others.
///
/// NOTE: compile-checked + SQL-reviewed, but **not** runtime-verified in CI
/// (no Postgres in the sandbox). `open_frontier` falls back to SQLite when
/// the server is unreachable, so a misconfigured `$CRABCC_CRAWL_PG` never
/// hard-fails a crawl.
#[cfg(feature = "crawl-postgres")]
pub struct PostgresFrontier {
    client: tokio_postgres::Client,
}

#[cfg(feature = "crawl-postgres")]
impl PostgresFrontier {
    /// Connect (libpq URL or key=value DSN), run the additive schema, and
    /// requeue any rows a crashed worker left `inflight`.
    pub async fn connect(conn_str: &str) -> anyhow::Result<Self> {
        let (client, connection) = tokio_postgres::connect(conn_str, tokio_postgres::NoTls).await?;
        // Drive the connection in the background; it resolves when the
        // client is dropped at end of crawl.
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::warn!(target: "crabcc_fetch", error = %e, "postgres connection closed");
            }
        });
        let me = Self { client };
        me.init().await?;
        Ok(me)
    }

    async fn init(&self) -> anyhow::Result<()> {
        self.client
            .batch_execute(
                "CREATE TABLE IF NOT EXISTS frontier (
                     url           TEXT PRIMARY KEY,
                     depth         INTEGER NOT NULL,
                     state         TEXT NOT NULL DEFAULT 'queued',
                     host          TEXT,
                     discovered_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint
                 );
                 CREATE INDEX IF NOT EXISTS frontier_state_depth ON frontier(state, depth);
                 CREATE TABLE IF NOT EXISTS pages (
                     url              TEXT PRIMARY KEY,
                     status           INTEGER,
                     title            TEXT,
                     content_markdown TEXT,
                     error            TEXT,
                     depth            INTEGER,
                     fetched_at       BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint
                 );
                 UPDATE frontier SET state='queued' WHERE state='inflight';",
            )
            .await?;
        Ok(())
    }

    async fn enqueue(&self, url: &str, depth: usize, host: Option<&str>) -> anyhow::Result<bool> {
        let n = self
            .client
            .execute(
                "INSERT INTO frontier(url, depth, state, host) VALUES ($1,$2,'queued',$3)
                 ON CONFLICT (url) DO NOTHING",
                &[&url, &(depth as i32), &host],
            )
            .await?;
        Ok(n > 0)
    }

    async fn claim(&self, limit: usize) -> anyhow::Result<Vec<Pending>> {
        // Atomic multi-worker claim: lock the shallowest queued rows,
        // skipping any another worker already holds, and flip them
        // inflight in one statement.
        let rows = self
            .client
            .query(
                "UPDATE frontier SET state='inflight'
                 WHERE url IN (
                     SELECT url FROM frontier WHERE state='queued'
                     ORDER BY depth, discovered_at
                     LIMIT $1
                     FOR UPDATE SKIP LOCKED
                 )
                 RETURNING url, depth",
                &[&(limit as i64)],
            )
            .await?;
        let mut out: Vec<Pending> = rows
            .iter()
            .map(|r| Pending {
                url: r.get::<_, String>(0),
                depth: r.get::<_, i32>(1) as usize,
            })
            .collect();
        out.sort_by_key(|p| p.depth);
        Ok(out)
    }

    async fn mark(&self, url: &str, state: &str) -> anyhow::Result<()> {
        self.client
            .execute("UPDATE frontier SET state=$2 WHERE url=$1", &[&url, &state])
            .await?;
        Ok(())
    }

    async fn record_page(&self, r: &FetchResult, depth: usize) -> anyhow::Result<()> {
        self.client
            .execute(
                "INSERT INTO pages(url,status,title,content_markdown,error,depth)
                 VALUES ($1,$2,$3,$4,$5,$6)
                 ON CONFLICT (url) DO UPDATE SET
                     status=EXCLUDED.status, title=EXCLUDED.title,
                     content_markdown=EXCLUDED.content_markdown,
                     error=EXCLUDED.error, depth=EXCLUDED.depth",
                &[
                    &r.url,
                    &(r.status as i32),
                    &r.title,
                    &r.content_markdown,
                    &r.error,
                    &(depth as i32),
                ],
            )
            .await?;
        Ok(())
    }

    async fn counts(&self) -> anyhow::Result<(usize, usize)> {
        let pages: i64 = self
            .client
            .query_one("SELECT COUNT(*) FROM pages", &[])
            .await?
            .get(0);
        let queued: i64 = self
            .client
            .query_one("SELECT COUNT(*) FROM frontier WHERE state='queued'", &[])
            .await?
            .get(0);
        Ok((pages as usize, queued as usize))
    }
}

/// Open the crawl frontier, preferring the shared Postgres backend and
/// falling back to a local SQLite file at `local_path`.
///
/// `pg_url` is the shared-queue connection string (typically from
/// `$CRABCC_CRAWL_PG`). When it's `Some` but the Postgres backend is
/// unreachable — or not compiled in — we log (host only; a DSN can embed
/// `user:password@`, which must never reach the logs) and drop to the
/// local SQLite frontier, so a crawl never hard-fails on missing infra.
pub async fn open_frontier(local_path: &Path, pg_url: Option<&str>) -> anyhow::Result<Frontier> {
    if let Some(pg) = pg_url {
        let redacted = url::Url::parse(pg)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| "<set>".to_string());
        #[cfg(feature = "crawl-postgres")]
        match PostgresFrontier::connect(pg).await {
            Ok(f) => {
                tracing::info!(
                    target: "crabcc_fetch",
                    pg_host = %redacted,
                    "using shared postgres crawl frontier",
                );
                return Ok(Frontier::Postgres(f));
            }
            Err(e) => tracing::warn!(
                target: "crabcc_fetch",
                pg_host = %redacted,
                error = %e,
                "postgres unreachable; using local SQLite frontier",
            ),
        }
        #[cfg(not(feature = "crawl-postgres"))]
        tracing::warn!(
            target: "crabcc_fetch",
            pg_host = %redacted,
            "postgres backend not compiled (build with --features crawl-postgres); using local SQLite frontier",
        );
    }
    Ok(Frontier::Sqlite(SqliteFrontier::open(local_path)?))
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
