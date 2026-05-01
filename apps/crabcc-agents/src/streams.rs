use anyhow::{Context, Result};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use tracing::warn;

use crate::config::Config;

/// XADD-based log fan-out. One Redis Stream per job id.
///
/// Schema (per entry):
///   - `s`  source: "stdout" | "stderr" | "event"
///   - `t`  ts: rfc3339
///   - `m`  message: utf-8 bytes (lossy on invalid)
///
/// Trimming: `MAXLEN ~ N` (approximate, fast). On EOF the runner
/// publishes a sentinel `s=event m=__eof__` so consumers can stop
/// XREAD-blocking cleanly.
#[derive(Clone)]
pub struct LogStreamer {
    conn: ConnectionManager,
    maxlen: usize,
    prefix: String,
}

impl LogStreamer {
    pub async fn connect(cfg: &Config) -> Result<Self> {
        let client = redis::Client::open(cfg.redis_url.as_str())
            .context("redis::Client::open")?;
        let conn = ConnectionManager::new(client)
            .await
            .context("ConnectionManager::new")?;
        Ok(Self {
            conn,
            maxlen: cfg.stream_maxlen,
            prefix: cfg.stream_key_prefix.clone(),
        })
    }

    pub fn key(&self, job_id: &str) -> String {
        format!("{}:{}", self.prefix, job_id)
    }

    pub async fn ping(&self) -> Result<()> {
        let mut c = self.conn.clone();
        let _: String = redis::cmd("PING")
            .query_async(&mut c)
            .await
            .context("redis PING")?;
        Ok(())
    }

    pub async fn append(&self, job_id: &str, source: Source, msg: &str) {
        let key = self.key(job_id);
        let ts = chrono::Utc::now().to_rfc3339();
        let mut c = self.conn.clone();
        let res: redis::RedisResult<String> = c
            .xadd_maxlen(
                &key,
                redis::streams::StreamMaxlen::Approx(self.maxlen),
                "*",
                &[("s", source.as_str()), ("t", &ts), ("m", msg)],
            )
            .await;
        if let Err(e) = res {
            warn!(job = %job_id, %e, "xadd failed");
        }
    }

    /// Sentinel terminator entry. Consumers see `s=event m=__eof__`.
    pub async fn finish(&self, job_id: &str, exit_code: i64) {
        self.append(job_id, Source::Event, &format!("__eof__ exit={exit_code}"))
            .await;
    }

    /// First-entry trackability event. Emitted right after `container
    /// started` so consumers reading from `0-0` see a single
    /// machine-parseable header packet before any stdout / stderr.
    /// `m` is `headers <json>` — the body parses as JSON without any
    /// further unwrapping. Empty headers are skipped.
    pub async fn append_headers(
        &self,
        job_id: &str,
        headers: &std::collections::HashMap<String, String>,
    ) {
        if headers.is_empty() {
            return;
        }
        match serde_json::to_string(headers) {
            Ok(json) => {
                self.append(job_id, Source::Event, &format!("headers {json}"))
                    .await;
            }
            Err(e) => {
                tracing::warn!(job = %job_id, %e, "headers serialize failed — dropping");
            }
        }
    }
}

#[derive(Copy, Clone)]
pub enum Source {
    Stdout,
    Stderr,
    Event,
}

impl Source {
    fn as_str(&self) -> &'static str {
        match self {
            Source::Stdout => "stdout",
            Source::Stderr => "stderr",
            Source::Event => "event",
        }
    }
}
