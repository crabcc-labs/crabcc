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
use std::path::PathBuf;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Holds the non-blocking writer's worker. Drop = flush.
///
/// Two writers are kept alive for the duration of the program: the
/// stderr pretty-printer (always-on) and the per-repo JSON file
/// appender (`<cwd>/.crabcc/telemetry.jsonl`, when the cwd is a
/// crabcc-indexed repo). The dashboard at `/live` tails the file via
/// `/api/telemetry`, so cross-process events from `crabcc graph
/// cycles`, `crabcc agent --run`, etc. all show up in one place.
pub struct TelemetryGuard {
    _writer: tracing_appender::non_blocking::WorkerGuard,
    _file_writer: Option<tracing_appender::non_blocking::WorkerGuard>,
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

    // Stderr layer (with the user-driven RUST_LOG filter). The
    // per-layer filter form lets the file layer (below) keep KPI
    // events even when stderr is narrowed via RUST_LOG=warn.
    let fmt_layer = fmt::layer()
        .with_writer(writer)
        .with_target(true)
        .with_ansi(io::stderr().is_terminal())
        .with_filter(filter);

    // Per-repo JSON file appender so the dashboard `/api/telemetry`
    // route + cross-process invocations share one stream of events.
    // Only set up when the cwd looks like a crabcc-indexed repo
    // (i.e. `.crabcc/` exists or can be created); otherwise skipped
    // so one-shot invocations from a non-repo dir don't pollute disk.
    let (file_layer, file_guard) = match telemetry_file_writer() {
        Some((_path, w, g)) => {
            // The file layer needs its OWN filter so it captures KPI
            // events even when the user has narrowed RUST_LOG for the
            // terminal. Without this, `RUST_LOG=warn` would silence
            // both stderr AND the file — defeating the dashboard's
            // reason to exist.
            let file_filter = EnvFilter::try_from_env("CRABCC_TELEMETRY_LOG")
                .unwrap_or_else(|_| {
                    EnvFilter::new(
                        "crabcc_mcp=info,crabcc_core::graph=info,crabcc_cli::agent=info",
                    )
                });
            let layer = fmt::layer()
                .json()
                .with_writer(w)
                .with_target(true)
                .with_current_span(false)
                .with_span_list(false)
                .with_filter(file_filter);
            (Some(layer), Some(g))
        }
        None => (None, None),
    };

    // `try_init` so a second call (e.g. from a test harness that
    // already wired tracing) doesn't panic.
    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(file_layer)
        .try_init();

    TelemetryGuard {
        _writer: guard,
        _file_writer: file_guard,
    }
}

/// Resolve `<cwd>/.crabcc/telemetry.jsonl`, create its parent if
/// missing, and return (path, non-blocking writer, guard). Skipped
/// when the cwd isn't writable (e.g. /tmp tests with a read-only
/// fixture).
fn telemetry_file_writer() -> Option<(
    PathBuf,
    tracing_appender::non_blocking::NonBlocking,
    tracing_appender::non_blocking::WorkerGuard,
)> {
    // Honor an explicit override (useful for the dashboard runs in a
    // different cwd than the repo it's serving).
    let path = std::env::var_os("CRABCC_TELEMETRY_FILE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|d| d.join(".crabcc").join("telemetry.jsonl"))
        })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    let (w, g) = tracing_appender::non_blocking(file);
    Some((path, w, g))
}
