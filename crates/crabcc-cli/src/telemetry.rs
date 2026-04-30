//! Telemetry init — issue #90.
//!
//! Single-call init for the workspace tracing pipeline. Called from
//! `main()` exactly once, early. Returns a [`TelemetryGuard`] that the
//! caller must keep alive until shutdown — dropping it flushes the
//! non-blocking writer's worker thread (matthieum rule: hot path
//! returns in O(ns); flush happens off-path).
//!
//! # Filter strategy
//!
//! Default in release builds is `crabcc=info,warn`:
//!   - `crabcc=info` surfaces our KPI events (MCP tool dispatch,
//!     graph build counts, agent run lifecycle) without burying the
//!     terminal in tantivy / sqlite chatter.
//!   - `warn` is the floor for everything else — third-party crates
//!     can still surface anomalies.
//!
//! Override at runtime with `RUST_LOG`:
//!   - `RUST_LOG=crabcc=debug crabcc agent --run …` — full crabcc trace
//!   - `RUST_LOG=warn crabcc index` — silent except for failures
//!   - `RUST_LOG=crabcc_mcp=info,crabcc_core=info crabcc --mcp` —
//!     surface only the KPI tags from the MCP + core paths.
//!
//! # KPI events
//!
//! What lands at the default `info` level (release build):
//!
//! | Site                                  | Fields                              |
//! |---------------------------------------|-------------------------------------|
//! | `crabcc_mcp::dispatch_tool_with`      | tool, elapsed_ms, ok\|error         |
//! | `crabcc_core::graph::CallGraph::build`| edges, nodes, duration_ms           |
//! | `crabcc_core::graph::CallGraph::cycles`| count, duration_ms                 |
//! | `crabcc_core::graph::CallGraph::orphans`| count, duration_ms                |
//! | `crabcc_core::graph::CallGraph::walk` | direction, depth, frontier_size     |
//! | `crabcc_cli::agent::sandbox.*`        | x_request_id, x_timings, cold/warm  |
//!
//! Anything chattier (per-file walks, per-symbol extracts, sqlite
//! statement traces) sits at `debug` and stays compiled out of the
//! release filter unless the user opts in.

use std::io::{self, IsTerminal};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Holds the non-blocking writer's worker. Drop = flush.
pub struct TelemetryGuard {
    _writer: tracing_appender::non_blocking::WorkerGuard,
}

/// Initialize the workspace tracing pipeline. Idempotent in the sense
/// that a second call returns a fresh guard but the first call's
/// subscriber is already global — subsequent registry init becomes a
/// no-op (tracing's `try_init`). The guard returned by the FIRST call
/// is the one that flushes; later guards do nothing useful.
pub fn init() -> TelemetryGuard {
    // Non-blocking writer over stderr. Workers run on a separate
    // thread; the call site returns as soon as the event is enqueued.
    let (writer, guard) = tracing_appender::non_blocking(io::stderr());

    // Default: KPI-only. Only the two paths that carry release-time
    // KPI events surface at `info`; everything else (incl. our own
    // chatter in main.rs / agent.rs / index.rs) stays at `warn`.
    // Override with `RUST_LOG=crabcc=debug` for full traces.
    //
    //   crabcc_mcp=info        → dispatch_tool_with: tool, elapsed_ms
    //   crabcc_core::graph=info → graph.build / cycles / orphans / walk
    //   warn                    → everything else (tantivy commits,
    //                             sqlite stmts, our generic info logs)
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("crabcc_mcp=info,crabcc_core::graph=info,warn"));

    let fmt_layer = fmt::layer()
        .with_writer(writer)
        .with_target(true)
        .with_ansi(io::stderr().is_terminal());

    // `try_init` so a second call (e.g. from a test harness that
    // already wired tracing) doesn't panic.
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .try_init();

    TelemetryGuard { _writer: guard }
}
