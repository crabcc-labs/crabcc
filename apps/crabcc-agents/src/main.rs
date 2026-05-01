//! crabcc-agents — BullMQ worker that runs Claude Code agents in
//! sandboxed Docker containers and streams logs back via Redis Streams.
//!
//! See `README.md` for architecture; see `apps/CONTAINER-POLICY.md` for
//! image policy.

// jemalloc — Linux-only. See Cargo.toml for the rationale.
#[cfg(all(not(target_env = "msvc"), target_os = "linux"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

mod config;
mod discovery;
mod health;
mod job;
mod litellm;
mod runner;
mod streams;

use anyhow::Result;
use bullmq_rs::{BullmqError, RedisConnection, WorkerBuilder};
use std::sync::Arc;
use tokio::signal;
use tracing::{error, info};

use crate::config::Config;
use crate::job::AgentJob;
use crate::runner::Runner;
use crate::streams::LogStreamer;

fn main() -> Result<()> {
    init_tracing();

    let cfg = Config::from_env()?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(cfg.tokio_worker_threads)
        .enable_all()
        .thread_name("crabcc-agents")
        .build()?;
    rt.block_on(async_main(cfg))
}

async fn async_main(cfg: Config) -> Result<()> {
    info!(queue = %cfg.queue_name, redis = %cfg.redis_url_redacted(), "boot");

    let runner = Arc::new(Runner::connect(&cfg).await?);
    if cfg.prewarm {
        runner.prewarm().await;
    }

    // LiteLLM preflight — fast TCP probe of the configured proxy.
    // Logs unreachable as a warning; hard-fails boot when
    // LITELLM_REQUIRED=1 so production deploys don't quietly run
    // without the proxy.
    litellm::ensure(&cfg).await?;

    let streamer = Arc::new(LogStreamer::connect(&cfg).await?);

    // Tiny HTTP /healthz so the container HEALTHCHECK can probe us.
    let health_handle = tokio::spawn(health::serve(
        cfg.health_addr,
        runner.clone(),
        streamer.clone(),
    ));

    let conn = RedisConnection::new(&cfg.redis_url);
    let worker = WorkerBuilder::new(&cfg.queue_name)
        .connection(conn)
        .concurrency(cfg.concurrency)
        .poll_interval(std::time::Duration::from_millis(cfg.poll_ms))
        .on_completed(|job| info!(job = %job.id, "completed"))
        .on_failed(|job, err| error!(job = %job.id, %err, "failed"))
        .build::<AgentJob>()
        .await?;

    let runner_h = runner.clone();
    let streamer_h = streamer.clone();
    let handle = worker
        .start(move |job| {
            let runner = runner_h.clone();
            let streamer = streamer_h.clone();
            async move {
                runner
                    .run(&job, streamer.as_ref())
                    .await
                    .map_err(|e| BullmqError::Other(e.to_string()))
            }
        })
        .await?;

    info!("worker running — Ctrl+C to stop");
    signal::ctrl_c().await?;
    info!("shutdown signal received");
    handle.shutdown();
    handle.wait().await?;
    health_handle.abort();
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,crabcc_agents=debug"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json())
        .init();
}
