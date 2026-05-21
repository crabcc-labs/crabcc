//! `crabcc-viz` — localhost call-graph visualizer for `crabcc serve`.
//!
//! Sync, threaded HTTP via `tiny_http`. No async runtime, no external CDN
//! requests, no JavaScript-build step. The frontend HTML at
//! `assets/index.html` is bundled into the binary with `include_str!`.
//!
//! Routes:
//!   GET /                              -> bundled HTML page
//!   GET /api/graph?root=&dir=&depth=   -> JSON snapshot of call-graph BFS
//!   GET /api/activity?since=&limit=    -> tail of ~/.crabcc/usage.log filtered
//!                                         to this repo (drives the live
//!                                         agent-activity overlay)
//!   GET /api/health                    -> `{ "status": "ok" }`
//!
//! "Live" today is implemented as 1.5s polling against `/api/activity` (a
//! single-user localhost dashboard doesn't need SSE/WebSocket, and tiny_http
//! doesn't have native WS support). Phase 2 promotes this to SSE — the
//! polling cadence is fast enough that human users perceive it as live.
//!
//! See issue #64 for the full design and follow-on slices (file-browser
//! sidebar, inspector pane, snapshot export, native SSE push).

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod banner;
mod bootstrap;
pub mod forge;
mod git_analytics;
mod graph;
mod memory_view;
mod query;
pub mod runtime;

use anyhow::{Context, Result};
use banner::print_banner;
use bootstrap::{bootstrap_snapshot, BootstrapSnapshot};
use crabcc_core::graph::CallGraph;
use crabcc_core::store::Store;
use graph::{graph_snapshot, EdgeOut, NodeOut};
use memory_view::memory_recent;
use query::url_decode;
use serde::Serialize;
use tiny_http::{Header, Method, Request, Response, Server};

const BUNDLED_INDEX: &str = include_str!("../assets/index.html");
// Phase 1 of #17 ships the React bundle as the dashboard. The legacy
// hand-rolled `assets/live.html` is kept on disk for one release as a
// reference and back-compat target — it's no longer referenced from
// the running server but the file documents the pre-rewrite contract.
//
// Regenerate after editing `web/src/`: `cd crates/crabcc-viz/web && bun run build`.
const BUNDLED_LIVE: &str = include_str!("../web/dist/live.html");

/// OpenAPI 3.1 source-of-truth for the `/live` HTTP API (issue #170 phase 0).
///
/// Hand-maintained at `crates/crabcc-viz/openapi.yaml` and consumed by
/// `crates/crabcc-viz/web` via `openapi-typescript` codegen at build
/// time. The drift test `openapi_yaml_lists_every_route` (in the
/// `tests` module of this file) asserts every route this crate
/// matches in `serve` shows up in the YAML as an `operationId`, so a
/// new endpoint without a schema entry fails CI.
pub const OPENAPI_YAML: &str = include_str!("../openapi.yaml");

/// Caps that defend a single-user localhost server from accidental fork-bombs
/// (`?depth=200` returning a 50k-node graph that locks up the page). Exposed
/// as constants so they show up in `crabcc serve --help` output once we wire
/// a `--max-depth` override.
pub const MAX_DEPTH: usize = 6;
pub const MAX_NODES: usize = 1500;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: IpAddr,
    pub port: u16,
    pub root: PathBuf,
    pub no_open: bool,
    /// If true, run `runtime::ensure_initialized` at startup so the live
    /// dashboard's first bootstrap call has real numbers (not zeros).
    /// Cheap on warm repos (one mtime sweep + sidecar load).
    pub init: bool,
}

impl Config {
    pub fn loopback(root: PathBuf, port: u16) -> Self {
        Self {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            root,
            no_open: true,
            init: true,
        }
    }
}

/// Boot the server and block until SIGINT (or `tiny_http` returns from
/// `incoming_requests`). Returns the bound `SocketAddr` only once the
/// server has shut down — for the smoke-test path where we need the
/// resolved port up-front, use `bind_listener` + `serve_with_listener`.
pub fn serve(cfg: Config) -> Result<()> {
    // Bootstrap before bind: failing to index a fresh repo *here* gives
    // a clearer error than a 500 on the first /api/bootstrap poll. We
    // print the outcome inside `print_banner` so the user sees what
    // landed without an extra log line.
    let init_outcome = if cfg.init {
        match runtime::ensure_initialized(&cfg.root) {
            Ok(o) => Some(o),
            Err(e) => {
                eprintln!("crabcc serve: warning — init failed: {e:#}");
                None
            }
        }
    } else {
        None
    };
    if let Ok(home) = runtime::home_dir() {
        // ~/.crabcc/bin: best-effort. Symlink failures (e.g. read-only
        // FS) shouldn't block server start; the user can still hit /
        // and /live and view a static graph.
        if let Err(e) = runtime::ensure_bin_dir(&home) {
            tracing::debug!("crabcc serve: ensure_bin_dir failed: {e:#}");
        }
    }

    let listener = bind_listener(cfg.bind, cfg.port)?;
    let addr = listener.local_addr()?;
    print_banner(&cfg, addr, init_outcome.as_ref());
    if !cfg.no_open {
        let url = format!("http://{}:{}", addr.ip(), addr.port());
        if let Err(e) = open_browser(&url) {
            tracing::debug!("browser auto-open skipped: {e}");
        }
    }
    serve_with_listener(listener, &cfg.root)
}

/// Reserve the requested port (or an ephemeral one when `port == 0`)
/// without yet starting the request loop. Used by tests to learn the
/// picked port before the server is taking traffic.
pub fn bind_listener(bind: IpAddr, port: u16) -> Result<TcpListener> {
    let addr = SocketAddr::new(bind, port);
    TcpListener::bind(addr).with_context(|| format!("failed to bind {addr}"))
}

/// Run the request loop on a pre-bound listener. Each request is dispatched
/// on a worker thread so a slow handler can't head-of-line-block the next
/// request — `tiny_http`'s default thread pool is fine for a single-user
/// localhost server.
pub fn serve_with_listener(listener: TcpListener, root: &Path) -> Result<()> {
    let server = Server::from_listener(listener, None)
        .map_err(|e| anyhow::anyhow!("tiny_http failed to wrap listener: {e}"))?;
    let server = Arc::new(server);
    let root = Arc::new(root.to_path_buf());

    for request in server.incoming_requests() {
        let root = Arc::clone(&root);
        // Spawn cheaply — handlers are short-lived (graph BFS is ~ms).
        // Workers exit when their request completes.
        std::thread::spawn(move || {
            if let Err(e) = handle(request, &root) {
                tracing::warn!("crabcc viz: handler error: {e:#}");
            }
        });
    }
    Ok(())
}

fn handle(request: Request, root: &Path) -> Result<()> {
    let method = request.method().clone();
    let url = request.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url.as_str(), ""),
    };

    // POST is reserved for launch + kill endpoints. Any other POST is 405.
    if method == Method::Post {
        if let Some(rest) = path.strip_prefix("/api/agents/") {
            if let Some(id) = rest.strip_suffix("/kill") {
                return match agent_kill(id) {
                    Ok(snap) => respond_json(request, &snap),
                    Err(e) => respond_status(request, 400, &format!("kill failed: {e}")),
                };
            }
        }
        return match path {
            "/api/agents/launch" => match agents_launch(request, root) {
                Ok(()) => Ok(()),
                Err(e) => Err(e),
            },
            "/api/random-query" => match random_query(request, root) {
                Ok(()) => Ok(()),
                Err(e) => Err(e),
            },
            "/api/reindex" => match reindex_pwd(root) {
                Ok(snap) => respond_json(request, &snap),
                Err(e) => respond_status(request, 500, &format!("reindex failed: {e}")),
            },
            "/api/memory/ingest" => memory_ingest(request, root),
            _ => respond_status(request, 405, "method not allowed"),
        };
    }
    if method != Method::Get {
        return respond_status(request, 405, "method not allowed");
    }

    // Path-prefix routing for per-agent endpoints.
    if let Some(rest) = path.strip_prefix("/api/agents/") {
        if let Some(id) = rest.strip_suffix("/log") {
            return match agent_log(id, query) {
                Ok(snap) => respond_json(request, &snap),
                Err(e) => respond_status(request, 404, &format!("log unavailable: {e}")),
            };
        }
        if let Some(id) = rest.strip_suffix("/tail") {
            return match agent_tail(id, query) {
                Ok(snap) => respond_json(request, &snap),
                Err(e) => respond_status(request, 404, &format!("tail unavailable: {e}")),
            };
        }
        if let Some(id) = rest.strip_suffix("/info") {
            return match agent_info(id) {
                Ok(snap) => respond_json(request, &snap),
                Err(e) => respond_status(request, 404, &format!("info unavailable: {e}")),
            };
        }
    }

    match path {
        // Live monitoring dashboard is the front-door for `crabcc serve`
        // — most users land here to watch agent activity in real time.
        // The interactive call-graph viewer lives at `/graph`; `/live`
        // stays as a back-compat alias for the old URL.
        "/" | "/index.html" | "/live" => respond_html(request, BUNDLED_LIVE),
        "/graph" => respond_html(request, BUNDLED_INDEX),
        "/api/events" => sse_events(request, root.to_path_buf()),
        "/api/health" => respond_json(request, &serde_json::json!({ "status": "ok" })),
        // #172 — surface the hand-maintained OpenAPI spec so the
        // forthcoming docs container (com.crabcc.docs.api) can render
        // it without a separate file copy.
        "/api/openapi.yaml" => respond_yaml(request, OPENAPI_YAML),
        "/api/graph" => match graph_snapshot(root, query) {
            Ok(snapshot) => respond_json(request, &snapshot),
            Err(e) => respond_status(request, 400, &format!("bad request: {e}")),
        },
        "/api/activity" => match activity_tail(root, query) {
            Ok(activity) => respond_json(request, &activity),
            Err(e) => respond_status(request, 400, &format!("bad request: {e}")),
        },
        "/api/bootstrap" => match bootstrap_snapshot(root) {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("bootstrap failed: {e}")),
        },
        "/api/seed-graph" => match seed_graph(root, query) {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("seed-graph failed: {e}")),
        },
        "/api/agents" => match agents_list() {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("agents list failed: {e}")),
        },
        "/api/agent-profiles" => match agent_profiles_list(root) {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("agent-profiles failed: {e}")),
        },
        "/api/agent-kills" => match agent_kills_list() {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("agent-kills failed: {e}")),
        },
        "/api/agent-models" => match agent_models_list() {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("agent-models failed: {e}")),
        },
        "/api/ollama-key" => match ollama_key_snapshot() {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("ollama-key failed: {e}")),
        },
        "/api/services" => {
            let report = crabcc_core::service_discovery::discover_all();
            respond_json(request, &report)
        }
        "/api/debug/dump" => match debug_dump(root) {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("dump failed: {e}")),
        },
        "/api/telemetry" => match telemetry_tail(root, query) {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("telemetry tail failed: {e}")),
        },
        // Issue #86 — rotel OTLP health probe.
        // Checks whether the configured OTLP endpoint is reachable by
        // hitting its /health path. Returns {"reachable":bool,"endpoint":url}.
        // Used by the /live dashboard panel to show the green/red pill.
        "/api/telemetry/otlp-health" => {
            let snap = otlp_health_probe();
            respond_json(request, &snap)
        }
        "/api/memory/recent" => match memory_recent(root, query) {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("memory snapshot failed: {e}")),
        },
        "/api/memory/graph" => match memory_graph(root, query) {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("memory graph failed: {e}")),
        },
        "/api/memory/get" => match memory_get(root, query) {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("memory get failed: {e}")),
        },
        // ── Forge (GitHub / Gitea) ──────────────────────────────────────────
        "/api/forge/config" => {
            let cfg = forge::forge_config(root);
            respond_json(request, &cfg)
        }
        "/api/forge/prs" => {
            let state = query_param(query, "state").unwrap_or_else(|| "open".into());
            let page: u32 = query_param(query, "page")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            match forge::list_prs(root, &state, page) {
                Ok(snap) => respond_json(request, &snap),
                Err(e) => respond_status(request, 400, &format!("forge prs failed: {e}")),
            }
        }
        // ── Analytics ───────────────────────────────────────────────────────
        "/api/analytics/hotspots" => {
            let limit: usize = query_param(query, "limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(50)
                .clamp(1, 200);
            let snap = git_analytics::analytics_snapshot(root, limit, 200);
            respond_json(request, &serde_json::json!({
                "hotspots": snap.hotspots,
                "head_sha": snap.head_sha,
                "computed_at": snap.computed_at,
                "total_commits_scanned": snap.total_commits_scanned,
                "total_files_seen": snap.total_files_seen,
            }))
        }
        "/api/analytics/deadcode" => {
            let limit: usize = query_param(query, "limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(100)
                .clamp(1, 500);
            let snap = git_analytics::analytics_snapshot(root, 50, limit);
            respond_json(request, &serde_json::json!({
                "dead_code": snap.dead_code,
                "head_sha": snap.head_sha,
                "computed_at": snap.computed_at,
            }))
        }
        _ => {
            // Path-parameter forge routes: /api/forge/prs/{number}[/impact]
            if let Some(rest) = path.strip_prefix("/api/forge/prs/") {
                if let Some(num_str) = rest.strip_suffix("/impact") {
                    if let Ok(number) = num_str.parse::<u64>() {
                        return match forge::pr_impact_graph(root, number) {
                            Ok(snap) => respond_json(request, &snap),
                            Err(e) => respond_status(request, 400, &format!("impact graph: {e}")),
                        };
                    }
                }
                // /api/forge/prs/{number} — single PR detail
                if let Ok(number) = rest.parse::<u64>() {
                    return match (forge::get_pr(root, number), forge::get_pr_files(root, number)) {
                        (Ok(pr), Ok(files)) => {
                            respond_json(request, &forge::PrDetail { pr, files })
                        }
                        (Err(e), _) | (_, Err(e)) => {
                            respond_status(request, 400, &format!("pr detail: {e}"))
                        }
                    };
                }
            }
            respond_status(request, 404, "not found")
        }
    }
}

/// One-shot "what has the agent been doing?" snapshot for the live overlay.
/// Filters `~/.crabcc/usage.log` (a global JSONL stream written by every
/// crabcc CLI / MCP query) down to the current repo and the entries newer
/// than the client's last cursor.
///
/// `since` is a Unix-epoch second value; the client persists the maximum
/// `ts` it has seen across polls and re-sends it as `since` on the next
/// request. `limit` caps the response size to keep the polling payload
/// bounded when the agent goes on a fuzzy-search bender.
#[derive(Serialize)]
struct ActivitySnapshot {
    repo: String,
    cursor: u64,
    events: Vec<ActivityEvent>,
}

#[derive(Serialize, Clone)]
struct ActivityEvent {
    ts: u64,
    op: String,
    query: String,
    results: usize,
    /// Agent run id when the originating `track::record` call ran
    /// inside an agent process. `None` for direct CLI / IDE / MCP
    /// calls. Forwarded straight from the on-disk usage.log entry —
    /// see `crabcc_core::track::Entry::agent_id` (#311).
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_id: Option<String>,
}

// =====================================================================
// OTLP health probe — issue #86
//
// Probes the OTLP endpoint configured via OTEL_EXPORTER_OTLP_ENDPOINT
// by hitting its /health path (rotel and most collectors expose this).
// Purely read-only; aggregated telemetry is NEVER written to any
// crabcc SQLite database (index.db / _internal.db / memory.db).

#[derive(Serialize)]
struct OtlpHealthSnapshot {
    reachable: bool,
    endpoint: String,
    error: Option<String>,
}

fn otlp_health_probe() -> OtlpHealthSnapshot {
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap_or_default();
    if endpoint.is_empty() {
        return OtlpHealthSnapshot {
            reachable: false,
            endpoint: String::new(),
            error: Some("OTEL_EXPORTER_OTLP_ENDPOINT not set".into()),
        };
    }

    // Try rotel's /health endpoint (5 s timeout, no external crate needed).
    let health_url = format!("{endpoint}/health");
    let ok = (|| -> Option<bool> {
        use std::net::TcpStream;
        use std::time::Duration;
        let url = health_url
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        let (host_port, _) = url.split_once('/').unwrap_or((url, ""));
        let addr = if host_port.contains(':') {
            host_port.to_owned()
        } else {
            format!("{host_port}:80")
        };
        Some(TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_secs(2)).is_ok())
    })()
    .unwrap_or(false);

    OtlpHealthSnapshot {
        reachable: ok,
        endpoint,
        error: if ok {
            None
        } else {
            Some("TCP connect failed".into())
        },
    }
}

// =====================================================================
// Telemetry tail — `<root>/.crabcc/telemetry.jsonl` written by every
// crabcc invocation via `tracing::info!` events through the JSON file
// layer in `crabcc-cli/src/telemetry.rs`. Each line is a structured
// tracing event (`{"timestamp":..,"level":..,"target":..,"fields":{..}}`).
// The dashboard tails it for "tool calls + graph stats" (issue #90).
// =====================================================================

#[derive(Serialize)]
struct TelemetrySnapshot {
    cursor: u64, // max ts seen, in unix seconds
    events: Vec<TelemetryEvent>,
    /// Surfaced for the dashboard "debug" pane: where the file is,
    /// how many lines we read, whether it's missing.
    source: TelemetrySource,
}

#[derive(Serialize, Default)]
struct TelemetrySource {
    path: String,
    lines_read: usize,
    bytes: u64,
    exists: bool,
}

#[derive(Serialize, Clone)]
struct TelemetryEvent {
    ts: u64,
    level: String,
    target: String,
    /// Free-form structured fields the producer attached. We pass the
    /// JSON through unmodified so the frontend can render whatever the
    /// producer included (kpi name, duration_ms, count, tool, etc.).
    fields: serde_json::Value,
}

const TELEMETRY_DEFAULT_LIMIT: usize = 100;
const TELEMETRY_MAX_LIMIT: usize = 1000;
const TELEMETRY_MAX_LINES: usize = 5000; // bound the parse work per call

fn telemetry_tail(root: &Path, query: &str) -> Result<TelemetrySnapshot> {
    let mut since: u64 = 0;
    let mut limit: usize = TELEMETRY_DEFAULT_LIMIT;
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "since" => since = v.parse::<u64>().unwrap_or(0),
            "limit" => {
                limit = v
                    .parse::<usize>()
                    .unwrap_or(TELEMETRY_DEFAULT_LIMIT)
                    .clamp(1, TELEMETRY_MAX_LIMIT)
            }
            _ => {}
        }
    }

    let path = root.join(".crabcc").join("telemetry.jsonl");
    let mut source = TelemetrySource {
        path: path.display().to_string(),
        ..Default::default()
    };

    if !path.exists() {
        return Ok(TelemetrySnapshot {
            cursor: since,
            events: Vec::new(),
            source,
        });
    }
    source.exists = true;
    let bytes = std::fs::read(&path)?;
    source.bytes = bytes.len() as u64;

    // The file is append-only. Tail the last TELEMETRY_MAX_LINES lines
    // (cheaper than parsing 100 MB of history every poll). Walk
    // backwards from EOF counting newlines; slice from that offset.
    let start = if bytes.len() < 1 << 20 {
        0
    } else {
        let mut nl_count = 0usize;
        let mut i = bytes.len();
        while i > 0 && nl_count < TELEMETRY_MAX_LINES {
            i -= 1;
            if bytes[i] == b'\n' {
                nl_count += 1;
            }
        }
        i + (if bytes[i] == b'\n' { 1 } else { 0 })
    };

    let mut events: Vec<TelemetryEvent> = Vec::new();
    for line in bytes[start..].split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        source.lines_read += 1;
        let v: serde_json::Value = match serde_json::from_slice(line) {
            Ok(v) => v,
            Err(_) => continue, // tolerate the occasional bad line
        };
        // The fmt::layer().json() shape:
        //   {"timestamp":"2026-04-30T08:36:43.674476Z","level":"INFO",
        //    "fields":{...},"target":"..."}
        let ts = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .map(parse_iso8601_unix)
            .unwrap_or(0);
        if ts < since {
            continue;
        }
        let level = v
            .get("level")
            .and_then(|l| l.as_str())
            .unwrap_or("INFO")
            .to_string();
        let target = v
            .get("target")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let fields = v
            .get("fields")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
        events.push(TelemetryEvent {
            ts,
            level,
            target,
            fields,
        });
    }

    events.sort_by_key(|e| e.ts);
    if events.len() > limit {
        let drop = events.len() - limit;
        events.drain(..drop);
    }
    let cursor = events.last().map(|e| e.ts).unwrap_or(since);
    Ok(TelemetrySnapshot {
        cursor,
        events,
        source,
    })
}

/// Parse `2026-04-30T08:36:43.674476Z` → unix seconds. Tracing uses a
/// fixed RFC-3339 shape, so a hand-rolled parser is fine and saves a
/// chrono / time dep. Fractional seconds are dropped; level granularity
/// is what the dashboard cares about.
fn parse_iso8601_unix(s: &str) -> u64 {
    if s.len() < 19 {
        return 0;
    }
    let bytes = s.as_bytes();
    let n = |i: usize, len: usize| -> u64 {
        let mut v = 0u64;
        for j in 0..len {
            let c = bytes[i + j];
            if !c.is_ascii_digit() {
                return 0;
            }
            v = v * 10 + (c - b'0') as u64;
        }
        v
    };
    let y = n(0, 4) as i64;
    let mo = n(5, 2) as i64;
    let d = n(8, 2) as i64;
    let h = n(11, 2);
    let mi = n(14, 2);
    let se = n(17, 2);
    // Days from civil date (Howard Hinnant's algorithm — same as the
    // sandbox helper but inlined to keep crabcc-viz dep-free).
    let y_ = if mo <= 2 { y - 1 } else { y };
    let era = y_.div_euclid(400);
    let yoe = (y_ - era * 400) as u64;
    let mo_u = mo as u64;
    let d_u = d as u64;
    let mp = if mo_u >= 3 { mo_u - 3 } else { mo_u + 9 };
    let doy = (153 * mp + 2) / 5 + d_u - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe as i64 - 719468;
    (days as u64) * 86400 + h * 3600 + mi * 60 + se
}

const ACTIVITY_DEFAULT_LIMIT: usize = 100;
const ACTIVITY_MAX_LIMIT: usize = 500;

fn activity_tail(root: &Path, query: &str) -> Result<ActivitySnapshot> {
    let q = parse_activity_query(query)?;
    let repo_label = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();
    // `read_log` parses the entire file. For a localhost single-user
    // dashboard polling at ~1Hz this is fine — the log lives in the user's
    // home dir and even 100k entries parse in single-digit ms. We can swap
    // in an mtime-aware tail if it ever shows up in a profile.
    let entries = crabcc_core::track::read_log().unwrap_or_default();
    let mut events: Vec<ActivityEvent> = entries
        .into_iter()
        .filter(|e| e.ts > q.since && (q.repo_filter.is_none() || e.repo == repo_label))
        .map(|e| ActivityEvent {
            ts: e.ts,
            op: e.op,
            query: e.query,
            results: e.results,
            agent_id: e.agent_id,
        })
        .collect();
    // The on-disk log is naturally append-ordered, but we re-sort defensively
    // so a clock skew or out-of-band write can't ship a non-monotonic batch
    // to the frontend (which uses the max ts as its cursor).
    events.sort_by_key(|e| e.ts);
    if events.len() > q.limit {
        let drop = events.len() - q.limit;
        events.drain(..drop);
    }
    let cursor = events.last().map(|e| e.ts).unwrap_or(q.since);
    Ok(ActivitySnapshot {
        repo: repo_label,
        cursor,
        events,
    })
}

struct ActivityQuery {
    since: u64,
    limit: usize,
    repo_filter: Option<()>,
}

fn parse_activity_query(raw: &str) -> Result<ActivityQuery> {
    let mut since = 0u64;
    let mut limit = ACTIVITY_DEFAULT_LIMIT;
    let mut repo_filter: Option<()> = Some(());
    for pair in raw.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = url_decode(v);
        match k {
            "since" => {
                since = v
                    .parse::<u64>()
                    .map_err(|_| anyhow::anyhow!("since must be a Unix-epoch second"))?;
            }
            "limit" => {
                limit = v
                    .parse::<usize>()
                    .map_err(|_| anyhow::anyhow!("limit must be a positive integer"))?
                    .clamp(1, ACTIVITY_MAX_LIMIT);
            }
            // Pass `repo=*` to disable the per-repo filter (useful when the
            // viewer is bound to a workspace root that doesn't match the
            // repo label recorded in usage.log entries).
            "repo" if v == "*" => {
                repo_filter = None;
            }
            _ => {}
        }
    }
    Ok(ActivityQuery {
        since,
        limit,
        repo_filter,
    })
}

// ── /api/seed-graph ─────────────────────────────────────────────────────
//
// "What should the live relation graph show before any agent has run?"
// Picks the top-degree nodes (combined in/out edges) from the cached
// `graph.json` and returns them with their immediate neighbors. Gives
// the live dashboard something meaningful to render on first paint
// instead of an empty canvas.

#[derive(Serialize)]
struct SeedSnapshot {
    nodes: Vec<NodeOut>,
    edges: Vec<EdgeOut>,
    seeds: Vec<String>,
}

fn seed_graph(root: &Path, query: &str) -> Result<SeedSnapshot> {
    let mut limit: usize = 8;
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == "limit" {
            limit = url_decode(v).parse::<usize>().unwrap_or(8).clamp(2, 32);
        }
    }

    let graph_path = root.join(".crabcc").join("graph.json");
    if !graph_path.exists() {
        // No cached graph — return an empty seed; the frontend just
        // shows the empty-state hint and waits for activity. We don't
        // fall back to building on the fly here because seed-graph
        // is on the page-boot critical path and a cold build can
        // take seconds on a real repo.
        return Ok(SeedSnapshot {
            nodes: vec![],
            edges: vec![],
            seeds: vec![],
        });
    }
    let graph = CallGraph::load(&graph_path)?;
    // Open the symbol store too — node enrichment (#301) needs it to
    // map each id to kind/file/line/signature. `Result` is propagated
    // because a missing store means no index, and an unenriched seed
    // graph is worse UX than the empty-state hint.
    let db = root.join(".crabcc").join("index.db");
    let store = Store::open(&db).with_context(|| format!("opening store at {}", db.display()))?;

    // Combined-degree ranking: a node's "importance" for the seed view
    // is the sum of its outgoing + incoming edge counts. This biases
    // toward central / heavily-traversed symbols, which are usually
    // the more interesting starting points than leaf-of-the-tree fns.
    let mut degree: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (k, v) in &graph.callees {
        *degree.entry(k.as_str()).or_insert(0) += v.len();
        for nb in v {
            *degree.entry(nb.as_str()).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<(&str, usize)> = degree.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let seeds: Vec<String> = ranked
        .iter()
        .take(limit)
        .map(|(s, _)| s.to_string())
        .collect();

    // Materialize the induced subgraph: for each seed, pull its direct
    // callers + callees and keep edges where both endpoints are in
    // the seed set OR are an immediate neighbor of one.
    let mut node_set: std::collections::HashSet<String> = seeds.iter().cloned().collect();
    for s in &seeds {
        if let Some(callees) = graph.callees.get(s) {
            for c in callees {
                node_set.insert(c.clone());
            }
        }
        if let Some(callers) = graph.callers.get(s) {
            for c in callers {
                node_set.insert(c.clone());
            }
        }
    }
    // Cap total nodes — really popular seeds blow up the snapshot
    // otherwise (one symbol with 200 callers floods the canvas).
    let cap = MAX_NODES.min(seeds.len() * 12);
    if node_set.len() > cap {
        // Keep the seeds first, then add neighbors deterministically
        // by sorted name until we hit `cap`.
        let mut out: std::collections::BTreeSet<String> = seeds.iter().cloned().collect();
        let mut others: Vec<&String> = node_set.iter().filter(|n| !out.contains(*n)).collect();
        others.sort();
        for n in others.into_iter().take(cap.saturating_sub(out.len())) {
            out.insert(n.clone());
        }
        node_set = out.into_iter().collect();
    }

    let nodes: Vec<NodeOut> = node_set
        .iter()
        .map(|id| {
            // Seeds are "depth 0" (queried-equivalent), neighbors are 1.
            let depth = if seeds.contains(id) { 0 } else { 1 };
            NodeOut::from_id_with_store(id.clone(), depth, &store)
        })
        .collect();
    let mut edges: Vec<EdgeOut> = Vec::new();
    for (src, dsts) in &graph.callees {
        if !node_set.contains(src) {
            continue;
        }
        for d in dsts {
            if node_set.contains(d) {
                edges.push(EdgeOut {
                    src: src.clone(),
                    dst: d.clone(),
                });
            }
        }
    }

    Ok(SeedSnapshot {
        nodes,
        edges,
        seeds,
    })
}

// ── /api/agents — list, log tail, launch ────────────────────────────────
//
// Surfaces `~/.crabcc/agents/<id>/` to the live dashboard. The dashboard
// can:
//   1. List recent runs (with status: in-flight if `lock` present, exited
//      otherwise; meta.json provides the start command + model + ts).
//   2. Tail a specific run's log via `/api/agents/<id>/log?since=N`.
//   3. POST `/api/agents/launch` with a JSON body to spawn a new run.
//
// Reads ~/.crabcc/agents/ directly rather than going through a DB; the
// directory is the source of truth and `crabcc agent --run` already
// writes the file shape we expect (lock, pid, log, meta.json).

#[derive(Serialize)]
struct AgentsList {
    agents: Vec<AgentSummary>,
}

#[derive(Serialize)]
struct AgentSummary {
    id: String,
    started_ts: u64,
    /// "running" if `lock` is still present, "exited" otherwise.
    status: &'static str,
    /// PID if `pid` file is present and parseable.
    pid: Option<u32>,
    runtime: String,
    model: Option<String>,
    /// Truncated start prompt (first 240 chars) — full prompt lives
    /// in `meta.json` if the user wants the rest.
    prompt_preview: String,
    /// Approximate log size in bytes (so the UI can show "12 KB").
    log_bytes: u64,
    /// Repo root the agent was started against, from meta.json.
    root: Option<String>,
}

/// Best-effort meta.json read with filesystem fallbacks. Used by both
/// `agents_list` and `agent_info` so they treat half-written run dirs,
/// `--dry-run` runs, and pre-`write_meta` racey snapshots identically.
///
/// Fallbacks:
///   * `started_ts == 0` (or no meta.json) → `lock` mtime → `dir` mtime.
///   * `pid` file content `"0"` (sentinel written before the real PID
///     lands) → return `None` so the UI doesn't render "pid 0".
struct ParsedMeta {
    started_ts: u64,
    runtime: String,
    model: Option<String>,
    prompt_preview: String,
    prompt_chars: usize,
    root: Option<String>,
}

fn read_agent_meta(dir: &std::path::Path) -> ParsedMeta {
    let meta_path = dir.join("meta.json");
    let mut started_ts = 0u64;
    let mut runtime_label = String::from("subprocess (host)");
    let mut model: Option<String> = None;
    let mut prompt_preview = String::new();
    let mut prompt_chars = 0usize;
    let mut root: Option<String> = None;
    if let Ok(body) = std::fs::read_to_string(&meta_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
            started_ts = v["started_ts"].as_u64().unwrap_or(0);
            runtime_label = v["runtime"].as_str().unwrap_or("?").to_string();
            model = v["model"].as_str().map(|s| s.to_string());
            prompt_preview = v["prompt_preview"].as_str().unwrap_or("").to_string();
            prompt_chars = v["prompt_chars"].as_u64().unwrap_or(0) as usize;
            root = v["root"].as_str().map(|s| s.to_string());
        }
    }
    if started_ts == 0 {
        // meta.json missing or pre-write_meta race: derive from
        // filesystem so the UI shows a real "started Xs ago" instead
        // of an em-dash. Try lock mtime first (created in
        // `RunDir::create`, before spawn); fall back to dir mtime.
        started_ts = mtime_secs(&dir.join("lock"))
            .or_else(|| mtime_secs(dir))
            .unwrap_or(0);
    }
    ParsedMeta {
        started_ts,
        runtime: runtime_label,
        model,
        prompt_preview,
        prompt_chars,
        root,
    }
}

fn read_agent_pid(pid_path: &std::path::Path) -> Option<u32> {
    let raw = std::fs::read_to_string(pid_path).ok()?;
    let n: u32 = raw.trim().parse().ok()?;
    // `0` is the sentinel written by `RunDir::create` before
    // `write_pid` lands the real PID — treat as "no pid yet".
    if n == 0 {
        None
    } else {
        Some(n)
    }
}

fn mtime_secs(p: &std::path::Path) -> Option<u64> {
    let modified = std::fs::metadata(p).ok()?.modified().ok()?;
    modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn agents_list() -> Result<AgentsList> {
    let home = runtime::home_dir()?;
    let dir = home.join(".crabcc").join("agents");
    let mut agents: Vec<AgentSummary> = vec![];
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Ok(AgentsList { agents: vec![] }),
    };
    for ent in entries.flatten() {
        let p = ent.path();
        if !p.is_dir() {
            continue;
        }
        let id = match p.file_name().and_then(|n| n.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let lock = p.join("lock");
        let pid_path = p.join("pid");
        let log_path = p.join("log");

        let status = if lock.exists() { "running" } else { "exited" };
        let pid = read_agent_pid(&pid_path);
        let log_bytes = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
        let meta = read_agent_meta(&p);
        agents.push(AgentSummary {
            id,
            started_ts: meta.started_ts,
            status,
            pid,
            runtime: meta.runtime,
            model: meta.model,
            prompt_preview: meta.prompt_preview,
            log_bytes,
            root: meta.root,
        });
    }
    // Most recent first; the dashboard shows running runs at the top.
    agents.sort_by_key(|a| std::cmp::Reverse(a.started_ts));
    Ok(AgentsList { agents })
}

#[derive(Serialize)]
struct AgentLog {
    id: String,
    cursor: u64,
    body: String,
    /// Total log size for "you've read X of Y bytes" UX.
    total: u64,
}

fn agent_log(id: &str, query: &str) -> Result<AgentLog> {
    // Defend against path traversal: the id segment is what got pulled
    // out of the URL between `/api/agents/` and `/log`. We require it
    // to be hex-only (the IDs we generate are 16 hex chars) to stop
    // anyone slipping a `..` past the URL parser.
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("invalid agent id");
    }
    let mut since: u64 = 0;
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == "since" {
            since = url_decode(v).parse().unwrap_or(0);
        }
    }
    let home = runtime::home_dir()?;
    let log_path = home.join(".crabcc").join("agents").join(id).join("log");
    if !log_path.exists() {
        anyhow::bail!("no such agent: {id}");
    }
    let total = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(&log_path)?;
    if since > 0 && since < total {
        f.seek(SeekFrom::Start(since))?;
    }
    // Cap the read at 256 KB per poll so a runaway agent doesn't make
    // the dashboard chew through gigabytes of stdout in one round-trip.
    // The frontend keeps polling, so the rest streams in over time.
    let cap = 256 * 1024usize;
    let mut buf = Vec::with_capacity(cap);
    f.take(cap as u64).read_to_end(&mut buf)?;
    let body = String::from_utf8_lossy(&buf).to_string();
    Ok(AgentLog {
        id: id.to_string(),
        cursor: since + buf.len() as u64,
        body,
        total,
    })
}

fn agents_launch(mut request: Request, root: &Path) -> Result<()> {
    // Parse JSON body: `{ "prompt": "...", "model"?, "profile"?, "no_refresh"? }`.
    let mut body = String::new();
    if let Err(e) = request.as_reader().read_to_string(&mut body) {
        return respond_status(request, 400, &format!("read body: {e}"));
    }
    #[derive(serde::Deserialize)]
    struct LaunchReq {
        prompt: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        no_refresh: bool,
        /// Profile id from `/api/agent-profiles` — bare filename
        /// without the `.profile.toml` suffix. Forwarded to the
        /// spawned CLI as `--profile internal/<id>`. Added in #306;
        /// `None` means "use the CLI's default".
        #[serde(default)]
        profile: Option<String>,
    }
    let req: LaunchReq = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => return respond_status(request, 400, &format!("invalid JSON: {e}")),
    };
    if req.prompt.trim().is_empty() {
        return respond_status(request, 400, "prompt must be non-empty");
    }
    // Validate the profile id shape before spawning. The CLI's
    // `--profile internal/<id>` parser accepts only `[A-Za-z0-9_-]`
    // and we pre-pend `internal/` here, so reject anything outside
    // that alphabet up-front — a child failure exits silently
    // (stderr is sunk to /dev/null) and the launch endpoint can't
    // surface it.
    if let Some(p) = req.profile.as_deref() {
        if p.is_empty()
            || !p
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return respond_status(
                request,
                400,
                &format!("profile id must match [A-Za-z0-9_-]+ (got '{p}')"),
            );
        }
    }

    // Spawn `crabcc agent --run …` as a detached subprocess so the
    // launch endpoint returns immediately. We capture the run id by
    // reading the most-recently-modified entry of ~/.crabcc/agents/
    // after the spawn — `crabcc agent` prints the id on stdout but
    // we'd have to keep the pipe open to see it; the directory
    // approach is simpler and matches what `agents_list` returns.
    let self_exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&self_exe);
    cmd.arg("--root").arg(root);
    cmd.arg("agent").arg("--run").arg(&req.prompt);
    if let Some(m) = &req.model {
        cmd.arg("--model").arg(m);
    }
    if let Some(p) = &req.profile {
        // Server-emitted profile ids are bare filenames; the CLI flag
        // wants the `internal/<id>` namespace prefix. Pre-pend here
        // so desktop / web clients don't need to know the namespace.
        cmd.arg("--profile").arg(format!("internal/{p}"));
    }
    if req.no_refresh {
        cmd.arg("--no-refresh");
    }
    // Detach: we don't wait, the agent's lifecycle is its run-dir.
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let before: std::collections::HashSet<String> = list_agent_ids().unwrap_or_default();
    // Detached + reaped + 20min hard timeout — see `spawn_detached`.
    let pid = spawn_detached(&mut cmd, Some(AGENT_HARD_TIMEOUT)).context("spawn `crabcc agent`")?;
    // Give the child a tick to create its run-dir, then diff to find
    // the new id. 200ms is long enough for `RunDir::create` to land
    // and short enough to not block the live dashboard's UI.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let after: std::collections::HashSet<String> = list_agent_ids().unwrap_or_default();
    let id = after.difference(&before).next().cloned();

    let response = serde_json::json!({
        "ok": true,
        "id": id,
        "pid": pid,
        "prompt_chars": req.prompt.chars().count(),
        "timeout_secs": AGENT_HARD_TIMEOUT.as_secs(),
    });
    respond_json(request, &response)
}

/// Last N lines of the agent log. Cheap inline preview for the agent
/// list — `agent_log` reads the whole file from `since` (good for
/// streaming the open-log-viewer pane); `agent_tail` reads the file
/// backwards in 8 KiB chunks until it has N newlines or hits the
/// start (good for "show me the latest 20 lines on every poll").
#[derive(Serialize)]
struct AgentTail {
    id: String,
    lines: Vec<String>,
    total: u64,
}

fn agent_tail(id: &str, query: &str) -> Result<AgentTail> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("invalid agent id");
    }
    let mut want_lines = 20usize;
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == "lines" {
            want_lines = url_decode(v).parse::<usize>().unwrap_or(20).clamp(1, 200);
        }
    }
    let home = runtime::home_dir()?;
    let log_path = home.join(".crabcc").join("agents").join(id).join("log");
    if !log_path.exists() {
        anyhow::bail!("no such agent: {id}");
    }
    let total = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(&log_path)?;
    // Read the last min(total, 64 KiB) bytes — enough for 20 lines of
    // typical agent output. Bigger logs scrollback through the streaming
    // log viewer (`/api/agents/<id>/log`) which is the right surface
    // for full-history reads.
    const WINDOW: u64 = 64 * 1024;
    let start = total.saturating_sub(WINDOW);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    // If we started mid-line, drop the leading partial.
    let text = if start > 0 {
        match text.find('\n') {
            Some(i) => &text[i + 1..],
            None => &text,
        }
    } else {
        &text
    };
    let all: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    let take = all.len().saturating_sub(want_lines);
    let lines = all.into_iter().skip(take).collect();
    Ok(AgentTail {
        id: id.to_string(),
        lines,
        total,
    })
}

#[derive(Serialize)]
struct AgentInfo {
    id: String,
    status: &'static str,
    pid: Option<u32>,
    is_alive: bool,
    started_ts: u64,
    runtime: String,
    model: Option<String>,
    prompt_chars: usize,
    prompt_preview: String,
    root: Option<String>,
    log_bytes: u64,
    paths: AgentPaths,
}

#[derive(Serialize)]
struct AgentPaths {
    dir: String,
    log: String,
    pid: String,
    lock: String,
    meta: String,
}

fn agent_info(id: &str) -> Result<AgentInfo> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("invalid agent id");
    }
    let home = runtime::home_dir()?;
    let dir = home.join(".crabcc").join("agents").join(id);
    if !dir.exists() {
        anyhow::bail!("no such agent: {id}");
    }
    let lock = dir.join("lock");
    let pid_path = dir.join("pid");
    let log_path = dir.join("log");
    let meta_path = dir.join("meta.json");

    let status = if lock.exists() { "running" } else { "exited" };
    let pid = read_agent_pid(&pid_path);
    let is_alive = pid.map(pid_alive).unwrap_or(false);
    let log_bytes = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
    let meta = read_agent_meta(&dir);

    Ok(AgentInfo {
        id: id.to_string(),
        status,
        pid,
        is_alive,
        started_ts: meta.started_ts,
        runtime: meta.runtime,
        model: meta.model,
        prompt_chars: meta.prompt_chars,
        prompt_preview: meta.prompt_preview,
        root: meta.root,
        log_bytes,
        paths: AgentPaths {
            dir: dir.display().to_string(),
            log: log_path.display().to_string(),
            pid: pid_path.display().to_string(),
            lock: lock.display().to_string(),
            meta: meta_path.display().to_string(),
        },
    })
}

#[derive(Serialize)]
struct KillResult {
    id: String,
    pid: Option<u32>,
    signaled: bool,
    note: String,
}

/// Send SIGTERM to the agent's pid (read from `pid` file). Best-effort:
/// the agent may have already exited (in which case the pid is reused
/// or stale); we never escalate to SIGKILL automatically — that's a
/// follow-up the user can do from a shell. The lock file is preserved
/// so `agents_list` correctly shows "running" until the child handles
/// the signal and exits (which removes its own lock).
fn agent_kill(id: &str) -> Result<KillResult> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("invalid agent id");
    }
    let home = runtime::home_dir()?;
    let pid_path = home.join(".crabcc").join("agents").join(id).join("pid");
    let pid: Option<u32> = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    let Some(pid) = pid else {
        return Ok(KillResult {
            id: id.to_string(),
            pid: None,
            signaled: false,
            note: "no pid file (agent may have exited or been a dry-run)".into(),
        });
    };
    if !pid_alive(pid) {
        return Ok(KillResult {
            id: id.to_string(),
            pid: Some(pid),
            signaled: false,
            note: "process already exited".into(),
        });
    }
    let signaled = send_sigterm(pid);
    Ok(KillResult {
        id: id.to_string(),
        pid: Some(pid),
        signaled,
        note: if signaled {
            "SIGTERM delivered; agent will clean up its run dir on exit"
        } else {
            "kill(pid, SIGTERM) failed (likely permissions)"
        }
        .into(),
    })
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // `kill(pid, 0)` returns 0 if the signal could be delivered without
    // actually delivering one — the standard "is this pid alive?" probe.
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid as i32, 0) == 0 }
}
#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn send_sigterm(pid: u32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    const SIGTERM: i32 = 15;
    unsafe { kill(pid as i32, SIGTERM) == 0 }
}
#[cfg(not(unix))]
fn send_sigterm(_pid: u32) -> bool {
    false
}

#[derive(Serialize)]
struct DebugDump {
    when: u64,
    bootstrap: BootstrapSnapshot,
    agents: AgentsList,
    activity: ActivitySnapshot,
}

/// One-shot debug snapshot — the dashboard's "dump debug" button
/// downloads this. Combines bootstrap + agent list + the last hour of
/// activity into a single JSON, suitable for attaching to a bug
/// report or a perf review thread.
// ----- agent-dashboard endpoints (issue #112 follow-up) -------------------

#[derive(Serialize)]
struct AgentProfile {
    id: String,
    crate_: Option<String>,
    description: Option<String>,
    model: Option<String>,
}

#[derive(Serialize)]
struct AgentProfilesList {
    dir: String,
    profiles: Vec<AgentProfile>,
}

/// List `internal_agents/*.profile.toml` files. Cheap directory walk
/// + per-file TOML parse. Empty list when the directory doesn't exist.
fn agent_profiles_list(root: &Path) -> Result<AgentProfilesList> {
    let dir = root.join("internal_agents");
    let mut out: Vec<AgentProfile> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            let id = match n.strip_suffix(".profile.toml") {
                Some(s) => s.to_string(),
                None => continue,
            };
            let body = std::fs::read_to_string(e.path()).unwrap_or_default();
            // Cheap field probe — avoid pulling toml as a viz dep just
            // for two strings. The frontend can call /api/agent-profiles
            // and re-parse if it needs the full schema.
            let crate_field = grep_toml_field(&body, "crate");
            let description = grep_toml_field(&body, "description");
            let model = grep_toml_field(&body, "model");
            out.push(AgentProfile {
                id,
                crate_: crate_field,
                description,
                model,
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(AgentProfilesList {
        dir: dir.display().to_string(),
        profiles: out,
    })
}

/// Pull `key = "value"` from a TOML body. Strict: skips section
/// headers, requires top-level key. Good enough for the dashboard's
/// "show description" surface; not a full TOML parser.
fn grep_toml_field(body: &str, key: &str) -> Option<String> {
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            return None; // entered a section — top-level only
        }
        if let Some(rest) = t.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim().trim_matches('"');
                return Some(rest.to_string());
            }
        }
    }
    None
}

#[derive(Serialize)]
struct AgentKillRow {
    run_id: String,
    reason: String,
    pid: Option<i64>,
    detail: Option<String>,
    killed_at: i64,
}

#[derive(Serialize)]
struct AgentKillsList {
    db: String,
    rows: Vec<AgentKillRow>,
}

/// Read the most recent rows from `agent_kill_events` in the singleton
/// `~/.crabcc/_internal.db`. Empty list when the DB doesn't exist yet.
fn agent_kills_list() -> Result<AgentKillsList> {
    let home = runtime::home_dir()?;
    let db_path = home.join(".crabcc").join("_internal.db");
    if !db_path.exists() {
        return Ok(AgentKillsList {
            db: db_path.display().to_string(),
            rows: vec![],
        });
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut stmt = conn.prepare(
        "SELECT run_id, reason, pid, detail, killed_at \
         FROM agent_kill_events ORDER BY killed_at DESC LIMIT 100",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(AgentKillRow {
                run_id: r.get(0)?,
                reason: r.get(1)?,
                pid: r.get(2)?,
                detail: r.get(3)?,
                killed_at: r.get(4)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(AgentKillsList {
        db: db_path.display().to_string(),
        rows,
    })
}

#[derive(Serialize)]
struct AgentModel {
    file: String,
    provider: String,
    name: String,
    params: Option<String>,
    context: Option<u64>,
    docs_first: Option<String>,
}

#[derive(Serialize)]
struct AgentModelsList {
    dir: String,
    models: Vec<AgentModel>,
}

/// Walk `$HOME/.crabcc/models/.model.*.info` and surface each entry.
/// Used by the dashboard's model picker. The exact filename
/// (`.model.<provider>.<name>.info`) is parsed back into provider /
/// name; we don't re-read the TOML body for the basic listing.
fn agent_models_list() -> Result<AgentModelsList> {
    let home = runtime::home_dir()?;
    let dir = home.join(".crabcc").join("models");
    let mut out: Vec<AgentModel> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let fname = e.file_name().to_string_lossy().to_string();
            // Pattern: .model.<provider>.<name>.info
            let stripped = match fname.strip_prefix(".model.") {
                Some(s) => s,
                None => continue,
            };
            let stripped = match stripped.strip_suffix(".info") {
                Some(s) => s,
                None => continue,
            };
            let mut parts = stripped.splitn(2, '.');
            let provider = parts.next().unwrap_or("?").to_string();
            let name = parts.next().unwrap_or("?").to_string();
            let body = std::fs::read_to_string(e.path()).unwrap_or_default();
            let params = grep_toml_field(&body, "params");
            let context = grep_toml_field(&body, "context").and_then(|s| s.parse().ok());
            let docs_first = grep_toml_array_first(&body, "docs");
            out.push(AgentModel {
                file: fname,
                provider,
                name,
                params,
                context,
                docs_first,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(AgentModelsList {
        dir: dir.display().to_string(),
        models: out,
    })
}

#[derive(Serialize)]
struct OllamaKeySnapshot {
    /// `present` = the file exists at `~/.crabcc.local.api-key`.
    present: bool,
    /// Absolute file path (always populated, even when missing).
    path: String,
    /// Mode-prefixed permissions string ("0400" / "0644" / …) so the
    /// frontend can flag a misconfigured (world-readable) key file.
    mode: Option<String>,
    /// Approximate file mtime in unix seconds — answers "when was
    /// this generated?" without leaking key bytes.
    mtime_secs: Option<u64>,
    /// File size in bytes — the key is short (one line). 0 = empty
    /// (broken state); >200 = something that's not a key (warn).
    size_bytes: Option<u64>,
    /// The actual key. Populated only when present + readable. The
    /// frontend masks it by default; the user clicks "reveal" to
    /// show. Loopback-only deployment + the file is already chmod
    /// 0400 in $HOME, so exposing here is no worse than `cat`.
    key: Option<String>,
}

fn ollama_key_snapshot() -> Result<OllamaKeySnapshot> {
    let home = runtime::home_dir()?;
    let path = home.join(".crabcc.local.api-key");
    let path_s = path.display().to_string();
    if !path.exists() {
        return Ok(OllamaKeySnapshot {
            present: false,
            path: path_s,
            mode: None,
            mtime_secs: None,
            size_bytes: None,
            key: None,
        });
    }
    let meta = std::fs::metadata(&path).ok();
    let size_bytes = meta.as_ref().map(|m| m.len());
    let mtime_secs = meta.as_ref().and_then(|m| {
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
    });
    #[cfg(unix)]
    let mode = meta.as_ref().map(|m| {
        use std::os::unix::fs::PermissionsExt;
        format!("{:04o}", m.permissions().mode() & 0o7777)
    });
    #[cfg(not(unix))]
    let mode: Option<String> = None;
    let key = std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string());
    Ok(OllamaKeySnapshot {
        present: true,
        path: path_s,
        mode,
        mtime_secs,
        size_bytes,
        key,
    })
}

/// Cheap scan for the first string in `key = ["..."]`. Used for the
/// model `docs` array surface; kept small + non-allocating-loop.
fn grep_toml_array_first(body: &str, key: &str) -> Option<String> {
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(key) {
            let rest = rest
                .trim_start()
                .strip_prefix('=')?
                .trim()
                .strip_prefix('[')?;
            let first = rest.split(',').next()?.trim();
            let first = first.trim_matches('"').trim_matches('\'');
            if !first.is_empty() && !first.starts_with(']') {
                return Some(first.to_string());
            }
        }
    }
    None
}

fn debug_dump(root: &Path) -> Result<DebugDump> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bootstrap = bootstrap_snapshot(root)?;
    let agents = agents_list()?;
    let since = now.saturating_sub(3600);
    let activity = activity_tail(root, &format!("since={since}&limit=500"))?;
    Ok(DebugDump {
        when: now,
        bootstrap,
        agents,
        activity,
    })
}

/// One-shot symbol query against a random op × random symbol drawn
/// from the cached `graph.json`. Used by the live dashboard's
/// "Run random query" button to populate the activity log + relation
/// graph without requiring the user to keep the simulator script
/// running. Cheap — the heaviest of the picked ops (`callers --count`)
/// is a single SQLite query.
fn random_query(_request: Request, root: &Path) -> Result<()> {
    let req = _request;
    // Pick from the same op set the live overlay treats as symbol-aware
    // (sym/refs/callers/outline). Outline takes a file, not a symbol —
    // we pick a random indexed file in that branch.
    let ops = ["sym", "refs", "callers"];
    let op = ops[(rand_usize() as usize) % ops.len()];

    // Random symbol from graph.json. We avoid hitting Store::find_by_name
    // because we want a name we know exists in the call-graph.
    let graph_path = root.join(".crabcc").join("graph.json");
    let mut symbols: Vec<String> = Vec::new();
    if let Ok(g) = CallGraph::load(&graph_path) {
        for k in g.callees.keys() {
            symbols.push(k.clone());
        }
    }
    if symbols.is_empty() {
        return respond_status(req, 400, "no graph.json — run `crabcc graph build` first");
    }
    let pick = &symbols[(rand_usize() as usize) % symbols.len()];

    let self_exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&self_exe);
    cmd.arg("--root").arg(root).arg(op).arg(pick);
    if op == "callers" || op == "refs" {
        cmd.arg("--count");
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    // No timeout: a single `crabcc <op> <name>` finishes in milliseconds.
    // We still go through `spawn_detached` so the wait thread reaps the
    // zombie (otherwise repeated random-query clicks would pile up
    // `<defunct>` entries in ps).
    spawn_detached(&mut cmd, None).with_context(|| format!("spawn `crabcc {op} {pick}`"))?;

    respond_json(
        req,
        &serde_json::json!({
            "ok": true,
            "op": op,
            "symbol": pick,
        }),
    )
}

/// Fast non-cryptographic RNG drawn from `/dev/urandom` (single-shot
/// per call). We don't pull `rand` for one usize.
fn rand_usize() -> u64 {
    use std::io::Read;
    let mut bytes = [0u8; 8];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_err()
    {
        // Fallback: time-based.
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        bytes.copy_from_slice(&ns.to_le_bytes());
    }
    u64::from_le_bytes(bytes)
}

/// Hard ceiling on agent run length, enforced by [`spawn_detached`]
/// when invoked with `Some(AGENT_HARD_TIMEOUT)`. Twenty minutes is the
/// default; long enough for a thoughtful refactor pass, short enough
/// to defend against a stuck agent (LLM rate-limit retry loops, MCP
/// transport hangs) burning hours of background time without human
/// input. Users who legitimately need a longer agent run can run
/// `crabcc agent --run …` directly from a shell — the hard timeout
/// only applies to dashboard-launched runs.
pub const AGENT_HARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20 * 60);

/// Spawn a child process and reap its zombie when it exits. Without
/// this, fire-and-forget spawns from `agents_launch` / `random_query`
/// would accumulate `<defunct>` entries in `ps` until our process
/// exits — Unix kernels keep an exited child's exit-status entry around
/// until the parent calls `waitpid` on it.
///
/// We solve this by handing each `Child` to a dedicated thread whose
/// only job is to call `child.wait()` (or poll `try_wait` when a
/// timeout is set) and then exit. No SIGCHLD handler, no global reaper
/// loop, no race with `agent_kill`'s SIGTERM — the wait thread sees
/// the signaled exit and reaps it just the same.
///
/// **Timeout semantics**: if `timeout` is `Some`, the reaper polls
/// `try_wait` every 5s; after the timeout elapses it sends SIGKILL
/// (via `Child::kill`) and then reaps. The dashboard sets this for
/// agent launches so a stuck `claude` process doesn't run forever in
/// the background. The kill is intentionally SIGKILL, not SIGTERM,
/// because the timeout is the *hard* fallback — userspace already
/// has [`agent_kill`] for the graceful path.
///
/// Returns the child's pid for callers that need it.
fn spawn_detached(
    cmd: &mut std::process::Command,
    timeout: Option<std::time::Duration>,
) -> Result<u32> {
    let child = cmd.spawn().context("spawn child process")?;
    let pid = child.id();
    std::thread::spawn(move || {
        let mut child = child;
        match timeout {
            None => {
                if let Err(e) = child.wait() {
                    tracing::debug!("crabcc viz: detached child {pid} wait failed: {e}");
                }
            }
            Some(deadline) => {
                let start = std::time::Instant::now();
                let poll = std::time::Duration::from_secs(5);
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => return,
                        Ok(None) => {
                            if start.elapsed() >= deadline {
                                tracing::warn!(
                                    "crabcc viz: agent pid {pid} hit {}s timeout — killing",
                                    deadline.as_secs()
                                );
                                let _ = child.kill();
                                let _ = child.wait();
                                return;
                            }
                            std::thread::sleep(poll);
                        }
                        Err(e) => {
                            tracing::debug!(
                                "crabcc viz: detached child {pid} try_wait failed: {e}"
                            );
                            return;
                        }
                    }
                }
            }
        }
    });
    Ok(pid)
}

fn list_agent_ids() -> Result<std::collections::HashSet<String>> {
    let home = runtime::home_dir()?;
    let dir = home.join(".crabcc").join("agents");
    let mut out = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            if let Some(name) = e.file_name().to_str() {
                out.insert(name.to_string());
            }
        }
    }
    Ok(out)
}

/// Extract a single query-string parameter by key.
fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == key {
            return Some(query::url_decode(v));
        }
    }
    None
}

fn respond_yaml(request: Request, body: &str) -> Result<()> {
    let mut resp = Response::from_string(body);
    resp.add_header(header("Content-Type", "application/yaml; charset=utf-8"));
    resp.add_header(header("X-Content-Type-Options", "nosniff"));
    resp.add_header(header("Cache-Control", "no-store"));
    request.respond(resp)?;
    Ok(())
}

fn respond_html(request: Request, body: &str) -> Result<()> {
    let mut resp = Response::from_string(body);
    resp.add_header(header("Content-Type", "text/html; charset=utf-8"));
    // Localhost-only viewer; lock down referrers + frame-busting + sniffing
    // so a stray phishing page on the same machine can't iframe us.
    resp.add_header(header("X-Content-Type-Options", "nosniff"));
    resp.add_header(header("X-Frame-Options", "DENY"));
    resp.add_header(header("Referrer-Policy", "no-referrer"));
    resp.add_header(header("Cache-Control", "no-store"));
    request.respond(resp)?;
    Ok(())
}

/// Server-Sent Events handler — collapses the polling triple
/// (`/api/activity`, `/api/agents`, `/api/memory/recent`) into one
/// long-lived HTTP response the React frontend subscribes to via
/// `EventSource`. Per-topic events are emitted as
/// `event: <topic>\ndata: <json>\n\n` blocks per the SSE spec.
///
/// Cadence is the same 1.5 / 2.5 second intervals the legacy poll loop
/// uses, but routed through one connection — fewer thread wakeups, no
/// per-request `Store::open`, and the React side can flip its "live"
/// indicator on `onopen`/`onerror` without a separate health probe.
///
/// Event types:
///   - `event: activity`  — `{items: ActivityHit[]}` (1.5s tick)
///   - `event: agents`    — `{agents: AgentSummary[]}` (2.5s tick)
///   - `event: ping`      — empty object every 15s; keeps the
///     connection alive through any reverse-proxy idle timeout.
fn sse_events(request: Request, root: std::path::PathBuf) -> Result<()> {
    use std::io::Write as _;
    let mut writer = request.into_writer();
    let header_block = "HTTP/1.1 200 OK\r\n\
                        Content-Type: text/event-stream; charset=utf-8\r\n\
                        Cache-Control: no-store\r\n\
                        Connection: keep-alive\r\n\
                        X-Accel-Buffering: no\r\n\r\n";
    writer.write_all(header_block.as_bytes())?;
    writer.flush()?;

    let mut last_activity = std::time::Instant::now();
    let mut last_agents = std::time::Instant::now();
    let mut last_ping = std::time::Instant::now();

    // Initial push so the client renders something on `onopen`.
    let _ = sse_emit(&mut writer, "activity", &activity_tail(&root, "").ok());
    let _ = sse_emit(&mut writer, "agents", &agents_list().ok());

    loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        let now = std::time::Instant::now();
        if now.duration_since(last_activity).as_millis() >= 1500 {
            last_activity = now;
            if sse_emit(&mut writer, "activity", &activity_tail(&root, "").ok()).is_err() {
                break;
            }
        }
        if now.duration_since(last_agents).as_millis() >= 2500 {
            last_agents = now;
            if sse_emit(&mut writer, "agents", &agents_list().ok()).is_err() {
                break;
            }
        }
        if now.duration_since(last_ping).as_secs() >= 15 {
            last_ping = now;
            if writer.write_all(b": ping\n\n").is_err() {
                break;
            }
            if writer.flush().is_err() {
                break;
            }
        }
    }
    Ok(())
}

fn sse_emit<W: std::io::Write, T: Serialize>(
    writer: &mut W,
    topic: &str,
    payload: &Option<T>,
) -> std::io::Result<()> {
    let body = match payload {
        Some(p) => sonic_rs::to_string(p).unwrap_or_else(|_| "null".into()),
        None => "null".into(),
    };
    write!(writer, "event: {topic}\ndata: {body}\n\n")?;
    writer.flush()
}

/// Run `crabcc index` against `root` and capture both the structured-JSON
/// stats (stdout) and the tracing log lines (stderr). Returns a single
/// `ReindexReport` JSON envelope so the UI panel can render counts,
/// duration, and per-stage log lines without a second round-trip.
fn reindex_pwd(root: &Path) -> Result<ReindexReport> {
    let self_exe = std::env::current_exe().context("locate self_exe")?;
    let started = std::time::Instant::now();
    let mut cmd = std::process::Command::new(&self_exe);
    cmd.arg("--root").arg(root);
    cmd.arg("index");
    // Scope the log filter so we surface our own info-level boundary
    // logs (full_index start/done, refresh deltas, query stats) and not
    // tantivy's per-commit chatter. RUST_LOG defaults to "warn", so an
    // unscoped subprocess would be silent.
    cmd.env(
        "RUST_LOG",
        "crabcc=info,crabcc_core=info,crabcc_cli=info,crabcc_mcp=info,crabcc_memory=info",
    );
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = cmd.output().context("spawn `crabcc index`")?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if !output.status.success() {
        anyhow::bail!(
            "crabcc index exited with {}: {}",
            output.status,
            stderr.lines().last().unwrap_or("(no stderr)")
        );
    }

    // stdout is the IndexStats JSON one-shot from main.rs.
    let stats: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| serde_json::json!({"raw_stdout": stdout.trim()}));

    // stderr is a stream of structured tracing lines. Cap the buffer so
    // a 100k-file reindex doesn't blow up the response payload — the UI
    // only renders the tail anyway.
    let logs: Vec<String> = stderr
        .lines()
        .map(|l| l.to_string())
        .rev()
        .take(MAX_REINDEX_LOG_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    Ok(ReindexReport {
        root: root.display().to_string(),
        elapsed_ms,
        stats,
        logs,
    })
}

const MAX_REINDEX_LOG_LINES: usize = 200;

#[derive(Serialize)]
struct ReindexReport {
    root: String,
    elapsed_ms: u64,
    stats: serde_json::Value,
    logs: Vec<String>,
}

fn respond_json<T: Serialize>(request: Request, value: &T) -> Result<()> {
    let body = sonic_rs::to_string(value)?;
    let mut resp = Response::from_string(body);
    resp.add_header(header("Content-Type", "application/json; charset=utf-8"));
    resp.add_header(header("Cache-Control", "no-store"));
    request.respond(resp)?;
    Ok(())
}

fn respond_status(request: Request, code: u16, msg: &str) -> Result<()> {
    let mut resp = Response::from_string(msg).with_status_code(code as i32);
    resp.add_header(header("Content-Type", "text/plain; charset=utf-8"));
    request.respond(resp)?;
    Ok(())
}

fn header(name: &str, value: &str) -> Header {
    // tiny_http's parser is permissive; this `unwrap` is fine for static
    // strings constructed by us in code (not user input).
    Header::from_bytes(name.as_bytes(), value.as_bytes())
        .expect("static HTTP header values must be valid")
}

#[cfg(target_os = "macos")]
fn open_browser(url: &str) -> Result<()> {
    std::process::Command::new("open").arg(url).status()?;
    Ok(())
}
#[cfg(target_os = "linux")]
fn open_browser(url: &str) -> Result<()> {
    std::process::Command::new("xdg-open").arg(url).status()?;
    Ok(())
}
#[cfg(target_os = "windows")]
fn open_browser(url: &str) -> Result<()> {
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .status()?;
    Ok(())
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn open_browser(_url: &str) -> Result<()> {
    Ok(())
}

// ── /api/memory/graph ───────────────────────────────────────────────────
//
// Knowledge-graph view of the memory drawers. Nodes are drawers (keyed
// by `source_id` since the integer PK is unstable across re-imports).
// Edges are explicit references mined from the body — `web:<hash>`,
// `text:<hash>`, `doc:<n>`, and Obsidian-style `[[Title]]` matched
// against drawer titles. Embedding-similarity edges aren't shipped
// here; the `embeddings` field in stats flips on once the
// `memory-vec` / `memory-embed` features are wired up at the consumer.

#[derive(Serialize)]
struct KnowledgeNode {
    id: String,
    title: String,
    kind: String,
    ts: i64,
    len: usize,
}

#[derive(Serialize)]
struct KnowledgeEdge {
    src: String,
    dst: String,
    via: &'static str,
}

#[derive(Serialize)]
struct KnowledgeStats {
    drawers: usize,
    edges: usize,
    embeddings: bool,
}

#[derive(Serialize)]
struct KnowledgeSnapshot {
    nodes: Vec<KnowledgeNode>,
    edges: Vec<KnowledgeEdge>,
    stats: KnowledgeStats,
}

fn memory_graph(root: &Path, query: &str) -> Result<KnowledgeSnapshot> {
    let mut limit: usize = 200;
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == "limit" {
            limit = url_decode(v).parse::<usize>().unwrap_or(200).clamp(1, 2000);
        }
    }
    let memory_path = root.join(".crabcc").join("memory.db");
    if !memory_path.exists() {
        return Ok(KnowledgeSnapshot {
            nodes: vec![],
            edges: vec![],
            stats: KnowledgeStats {
                drawers: 0,
                edges: 0,
                embeddings: false,
            },
        });
    }
    let conn = rusqlite::Connection::open_with_flags(
        &memory_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut stmt = conn.prepare(
        "SELECT d.source_id, w.name, d.body, d.created_at \
         FROM drawers d \
         LEFT JOIN wings w ON w.id = d.wing_id \
         WHERE d.body_enc = 0 \
         ORDER BY d.created_at DESC \
         LIMIT ?1",
    )?;
    type Row = (String, String, String, i64);
    let rows = stmt.query_map(rusqlite::params![limit as i64], |r| {
        Ok::<Row, rusqlite::Error>((
            r.get(0)?,
            r.get::<_, Option<String>>(1)?.unwrap_or_else(|| "?".into()),
            r.get(2)?,
            r.get(3)?,
        ))
    })?;
    let materialized: Vec<Row> = rows.filter_map(|r| r.ok()).collect();

    // First pass: build nodes + a title index for `[[wiki]]` resolution.
    let mut nodes: Vec<KnowledgeNode> = Vec::with_capacity(materialized.len());
    let mut id_set: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(materialized.len());
    let mut title_index: std::collections::HashMap<String, String> =
        std::collections::HashMap::with_capacity(materialized.len());
    for (source_id, wing, body, created_at) in &materialized {
        let title = first_line(body);
        title_index.insert(title.to_lowercase(), source_id.clone());
        id_set.insert(source_id.clone());
        nodes.push(KnowledgeNode {
            id: source_id.clone(),
            title,
            kind: wing.clone(),
            ts: *created_at,
            len: body.len(),
        });
    }

    // Second pass: parse references out of each body.
    let mut edges: Vec<KnowledgeEdge> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String, &'static str)> =
        std::collections::HashSet::new();
    for (source_id, _wing, body, _ts) in &materialized {
        for cand in scan_refs(body) {
            // Resolve candidate to a known drawer id.
            let (dst, via) = match cand {
                RefCand::Id(id) if id_set.contains(&id) => (id, "ref"),
                RefCand::Wiki(name) => match title_index.get(&name.to_lowercase()) {
                    Some(target) if target != source_id => (target.clone(), "wiki"),
                    _ => continue,
                },
                _ => continue,
            };
            if dst == *source_id {
                continue;
            }
            let key = (source_id.clone(), dst.clone(), via);
            if seen.insert(key) {
                edges.push(KnowledgeEdge {
                    src: source_id.clone(),
                    dst,
                    via,
                });
            }
        }
    }

    let drawer_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM drawers", [], |r| r.get(0))
        .unwrap_or(0);

    Ok(KnowledgeSnapshot {
        stats: KnowledgeStats {
            drawers: drawer_count.max(0) as usize,
            edges: edges.len(),
            embeddings: false,
        },
        nodes,
        edges,
    })
}

fn first_line(body: &str) -> String {
    body.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim_start_matches(['#', ' '])
        .chars()
        .take(80)
        .collect()
}

enum RefCand {
    Id(String),
    Wiki(String),
}

/// Tolerant reference scanner. Matches `web:<hex>`, `text:<hex>`,
/// `doc:<n>`, and Obsidian-style `[[Title]]`. Avoids regex (no `regex`
/// dep here) — hand-rolled byte scan is plenty for drawer-body sizes.
fn scan_refs(body: &str) -> Vec<RefCand> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // `[[...]]` wiki link.
        if bytes[i] == b'[' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            let start = i + 2;
            let mut end = start;
            while end + 1 < bytes.len() && !(bytes[end] == b']' && bytes[end + 1] == b']') {
                end += 1;
            }
            if end + 1 < bytes.len() {
                if let Ok(name) = std::str::from_utf8(&bytes[start..end]) {
                    let trimmed = name.trim();
                    if !trimmed.is_empty() && trimmed.len() < 200 {
                        out.push(RefCand::Wiki(trimmed.to_string()));
                    }
                }
                i = end + 2;
                continue;
            }
        }
        // `prefix:value` IDs — anchor on the prefix to avoid stripping
        // inline `http://` URLs.
        for prefix in ["web:", "text:", "doc:"] {
            if bytes[i..].starts_with(prefix.as_bytes()) {
                let start = i;
                let mut end = start + prefix.len();
                while end < bytes.len() {
                    let b = bytes[end];
                    if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                        end += 1;
                    } else {
                        break;
                    }
                }
                if end > start + prefix.len() {
                    if let Ok(id) = std::str::from_utf8(&bytes[start..end]) {
                        out.push(RefCand::Id(id.to_string()));
                    }
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

// ── /api/memory/get ─────────────────────────────────────────────────────
//
// Single-drawer fetch. Looks the drawer up by `source_id` (the stable
// human-readable identifier; the SQLite PK isn't stable across imports).

#[derive(Serialize)]
struct DrawerDetail {
    found: bool,
    id: String,
    wing: String,
    room: Option<String>,
    source_id: String,
    body: String,
    created_at: i64,
}

fn memory_get(root: &Path, query: &str) -> Result<DrawerDetail> {
    let mut id = String::new();
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == "id" {
            id = url_decode(v);
        }
    }
    let memory_path = root.join(".crabcc").join("memory.db");
    if id.is_empty() || !memory_path.exists() {
        return Ok(DrawerDetail {
            found: false,
            id: id.clone(),
            wing: String::new(),
            room: None,
            source_id: id,
            body: String::new(),
            created_at: 0,
        });
    }
    let conn = rusqlite::Connection::open_with_flags(
        &memory_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let row = conn.query_row(
        "SELECT d.source_id, w.name, r.name, d.body, d.created_at \
         FROM drawers d \
         LEFT JOIN wings w ON w.id = d.wing_id \
         LEFT JOIN rooms r ON r.id = d.room_id \
         WHERE d.source_id = ?1 AND d.body_enc = 0 \
         LIMIT 1",
        rusqlite::params![id],
        |r| {
            Ok::<(String, Option<String>, Option<String>, String, i64), rusqlite::Error>((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
            ))
        },
    );
    Ok(match row {
        Ok((source_id, wing, room, body, created_at)) => DrawerDetail {
            found: true,
            id: source_id.clone(),
            wing: wing.unwrap_or_else(|| "?".into()),
            room,
            source_id,
            body,
            created_at,
        },
        Err(_) => DrawerDetail {
            found: false,
            id: id.clone(),
            wing: String::new(),
            room: None,
            source_id: id,
            body: String::new(),
            created_at: 0,
        },
    })
}

// ── POST /api/memory/ingest ─────────────────────────────────────────────
//
// Pipe URLs and freeform text into the memory layer. URLs are fetched
// through `crabcc-fetch` (SSRF-checked, html→md cleaned), then stored
// as drawers via `crabcc-memory::Palace`. Idempotent: re-ingesting the
// same URL produces the same `web:<hash>` source_id, and the underlying
// `Palace::remember` path upserts on `source_id` collision.

#[derive(serde::Deserialize)]
struct IngestRequest {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    urls: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Serialize)]
struct IngestItem {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    bytes: usize,
    drawer_id: i64,
}

#[derive(Serialize)]
struct IngestError {
    url: String,
    error: String,
}

#[derive(Serialize)]
struct IngestStats {
    ok: usize,
    failed: usize,
}

#[derive(Serialize)]
struct IngestResponse {
    ingested: Vec<IngestItem>,
    errors: Vec<IngestError>,
    stats: IngestStats,
}

fn memory_ingest(mut request: Request, root: &Path) -> Result<()> {
    // Read body up to a generous cap — the JSON envelope is small.
    const MAX_BODY: usize = 1024 * 1024;
    let mut body = Vec::with_capacity(8 * 1024);
    {
        let reader = request.as_reader();
        let mut buf = [0u8; 8 * 1024];
        loop {
            let n = std::io::Read::read(reader, &mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n]);
            if body.len() > MAX_BODY {
                return respond_status(request, 413, "ingest body too large");
            }
        }
    }
    let req: IngestRequest = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return respond_status(request, 400, &format!("bad json: {e}")),
    };

    // De-dup URL set: explicit + linkified-from-text.
    let mut url_set: std::collections::BTreeSet<String> = req.urls.into_iter().collect();
    let raw_text = req.text.clone().unwrap_or_default();
    if !raw_text.is_empty() {
        for u in crabcc_fetch::extract_urls(&raw_text) {
            url_set.insert(u);
        }
    }
    let urls: Vec<String> = url_set.into_iter().collect();

    let source_label = req.source.unwrap_or_else(|| "web-ingest".to_string());
    let _ = req.tags; // tags reserved for future drawer-level metadata.

    let memory_path = root.join(".crabcc").join("memory.db");
    let palace = match crabcc_memory::Palace::open(root) {
        Ok(p) => p,
        Err(e) => return respond_status(request, 500, &format!("open palace: {e}")),
    };

    let mut ingested: Vec<IngestItem> = Vec::new();
    let mut errors: Vec<IngestError> = Vec::new();

    // URL fetch phase — async via a per-request runtime. Single-user
    // localhost so the runtime cost is negligible.
    if !urls.is_empty() {
        let safe: Vec<String> = urls
            .iter()
            .filter(|u| match crabcc_fetch::is_ingest_safe_url(u) {
                Ok(()) => true,
                Err(reason) => {
                    errors.push(IngestError {
                        url: (*u).clone(),
                        error: reason,
                    });
                    false
                }
            })
            .cloned()
            .collect();
        if !safe.is_empty() {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let results = rt.block_on(crabcc_fetch::fetch_and_clean(
                &safe,
                crabcc_fetch::FetchOpts::ingest(),
            ));
            for r in results {
                if r.error.is_some() || r.content_markdown.is_none() {
                    errors.push(IngestError {
                        url: r.url.clone(),
                        error: r.error.unwrap_or_else(|| "no content extracted".into()),
                    });
                    continue;
                }
                let body = r.content_markdown.unwrap_or_default();
                let id = format!("web:{}", short_hash(r.url.as_bytes()));
                match palace.remember(&source_label, None, &id, &body) {
                    Ok(drawer_id) => {
                        ingested.push(IngestItem {
                            id: id.clone(),
                            url: Some(r.url),
                            title: r.title,
                            kind: Some("web".into()),
                            bytes: body.len(),
                            drawer_id: drawer_id_as_i64(drawer_id),
                        });
                    }
                    Err(e) => errors.push(IngestError {
                        url: id,
                        error: format!("{e}"),
                    }),
                }
            }
        }
    }

    // Standalone-text path: only if URL extraction didn't already
    // consume the whole text (i.e. there's still content beyond URLs).
    if !raw_text.trim().is_empty() {
        let stripped = strip_urls(&raw_text);
        if !stripped.trim().is_empty() {
            let id = format!("text:{}", short_hash(raw_text.as_bytes()));
            let label = format!("{source_label}:text");
            match palace.remember(&label, None, &id, &raw_text) {
                Ok(drawer_id) => {
                    ingested.push(IngestItem {
                        id: id.clone(),
                        url: None,
                        title: None,
                        kind: Some("text".into()),
                        bytes: raw_text.len(),
                        drawer_id: drawer_id_as_i64(drawer_id),
                    });
                }
                Err(e) => errors.push(IngestError {
                    url: id,
                    error: format!("{e}"),
                }),
            }
        }
    }

    let _ = memory_path; // silences unused-binding when path validation moves later.
    respond_json(
        request,
        &IngestResponse {
            stats: IngestStats {
                ok: ingested.len(),
                failed: errors.len(),
            },
            ingested,
            errors,
        },
    )
}

fn short_hash(b: &[u8]) -> String {
    // Stable 64-bit FNV-style hash. Drawer source-ids are an
    // application-level identity key, not a security boundary, so a
    // cheap non-crypto hash is fine. Using `DefaultHasher` would be a
    // randomized SipHash → unstable across processes.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    let mut s = String::with_capacity(16);
    for i in (0..16).rev() {
        let nibble = ((h >> (i * 4)) & 0xf) as u8;
        s.push(if nibble < 10 {
            (b'0' + nibble) as char
        } else {
            (b'a' + nibble - 10) as char
        });
    }
    s
}

fn drawer_id_as_i64(id: crabcc_memory::DrawerId) -> i64 {
    // `DrawerId` is a newtype around the SQLite PK. Cast via `Into`
    // when available, else parse the Debug repr — both safe.
    let dbg = format!("{id:?}");
    dbg.trim_matches(|c: char| !c.is_ascii_digit())
        .parse::<i64>()
        .unwrap_or(0)
}

fn strip_urls(text: &str) -> String {
    let mut finder = crabcc_fetch::linkify::LinkFinder::new();
    finder.kinds(&[crabcc_fetch::linkify::LinkKind::Url]);
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for span in finder.spans(text) {
        if span.kind() == Some(&crabcc_fetch::linkify::LinkKind::Url) {
            out.push_str(&text[last..span.start()]);
            last = span.end();
        }
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod agent_meta_tests {
    //! Tests for the meta.json fallbacks added so the dashboard never
    //! renders all-em-dash agent rows during the (sub-second) window
    //! between `RunDir::create` and `write_meta`. The fallback chain is:
    //!   1. read meta.json verbatim (happy path);
    //!   2. on missing / pre-write_meta, derive `started_ts` from
    //!      `lock` mtime → dir mtime;
    //!   3. for the pid file, treat the literal `0` sentinel as None.

    use super::*;
    use std::fs;
    use std::path::Path;

    fn touch(p: &Path) {
        fs::write(p, "").unwrap();
    }

    #[test]
    fn read_agent_pid_zero_sentinel_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        fs::write(&pid, "0\n").unwrap();
        assert_eq!(read_agent_pid(&pid), None);
    }

    #[test]
    fn read_agent_pid_empty_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        fs::write(&pid, "").unwrap();
        assert_eq!(read_agent_pid(&pid), None);
    }

    #[test]
    fn read_agent_pid_garbage_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        fs::write(&pid, "not-a-number\n").unwrap();
        assert_eq!(read_agent_pid(&pid), None);
    }

    #[test]
    fn read_agent_pid_real_value() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        fs::write(&pid, "12345\n").unwrap();
        assert_eq!(read_agent_pid(&pid), Some(12345));
    }

    #[test]
    fn read_agent_pid_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_agent_pid(&dir.path().join("pid")), None);
    }

    #[test]
    fn read_agent_meta_falls_back_to_lock_mtime_when_meta_missing() {
        let dir = tempfile::tempdir().unwrap();
        // No meta.json — simulate the race window before write_meta.
        // Just touch a `lock` file; its mtime should drive started_ts.
        touch(&dir.path().join("lock"));
        let meta = read_agent_meta(dir.path());
        // We can't predict the exact unix-second the test runs at, but
        // it must be > 0 (i.e. SOMETHING was derived) and within the
        // last few minutes.
        assert!(meta.started_ts > 0, "started_ts not derived from mtime");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(
            (now.saturating_sub(meta.started_ts)) < 60,
            "started_ts={} is more than 60s before now={}",
            meta.started_ts,
            now
        );
        // Default runtime label so the UI always has SOMETHING readable.
        assert_eq!(meta.runtime, "subprocess (host)");
        assert!(meta.model.is_none());
        assert!(meta.root.is_none());
    }

    #[test]
    fn read_agent_meta_falls_back_to_dir_mtime_when_no_lock_either() {
        let dir = tempfile::tempdir().unwrap();
        let meta = read_agent_meta(dir.path());
        assert!(meta.started_ts > 0, "no fallback to dir mtime");
    }

    #[test]
    fn read_agent_meta_happy_path_uses_meta_json() {
        let dir = tempfile::tempdir().unwrap();
        let meta_json = serde_json::json!({
            "id": "abc",
            "started_ts": 1_700_000_000u64,
            "runtime": "subprocess (host)",
            "model": "claude-sonnet-4-6",
            "prompt_preview": "hello world",
            "prompt_chars": 11,
            "root": "/repo/foo",
        });
        fs::write(
            dir.path().join("meta.json"),
            serde_json::to_string(&meta_json).unwrap(),
        )
        .unwrap();
        // Even though there's no lock file, started_ts comes from json.
        let meta = read_agent_meta(dir.path());
        assert_eq!(meta.started_ts, 1_700_000_000);
        assert_eq!(meta.runtime, "subprocess (host)");
        assert_eq!(meta.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(meta.prompt_preview, "hello world");
        assert_eq!(meta.prompt_chars, 11);
        assert_eq!(meta.root.as_deref(), Some("/repo/foo"));
    }

    #[test]
    fn read_agent_meta_partial_json_still_falls_back_for_started_ts() {
        // meta.json present but missing started_ts — still derive
        // from filesystem so the row never shows "—".
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("lock"));
        fs::write(
            dir.path().join("meta.json"),
            r#"{"runtime": "subprocess (host)", "model": "x"}"#,
        )
        .unwrap();
        let meta = read_agent_meta(dir.path());
        assert!(meta.started_ts > 0);
        assert_eq!(meta.model.as_deref(), Some("x"));
    }

    #[test]
    fn mtime_secs_returns_some_for_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("touch");
        touch(&p);
        let t = mtime_secs(&p).expect("mtime should be readable");
        assert!(t > 0);
    }

    #[test]
    fn mtime_secs_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(mtime_secs(&dir.path().join("nope")).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::query::parse_query;
    use super::*;

    /// Routes the `serve` HTTP handler matches today. Hand-maintained
    /// alongside the match arms — adding a new `/api/...` endpoint
    /// requires adding it here AND in `crates/crabcc-viz/openapi.yaml`.
    /// The drift test below ties the two together so CI catches a
    /// missing schema update.
    ///
    /// Path-parameter routes appear with their OpenAPI `{id}` form,
    /// not the runtime `strip_suffix("/log")` form, so this list
    /// matches the YAML's `paths:` keys verbatim.
    const KNOWN_API_PATHS: &[&str] = &[
        "/api/health",
        "/api/openapi.yaml",
        "/api/bootstrap",
        "/api/activity",
        "/api/agents",
        "/api/agents/{id}/log",
        "/api/agents/{id}/tail",
        "/api/agents/{id}/info",
        "/api/agents/{id}/kill",
        "/api/agents/launch",
        "/api/agent-profiles",
        "/api/agent-kills",
        "/api/agent-models",
        "/api/ollama-key",
        "/api/services",
        "/api/telemetry",
        "/api/telemetry/otlp-health",
        "/api/reindex",
        "/api/random-query",
        "/api/graph",
        "/api/seed-graph",
        "/api/debug/dump",
        "/api/memory/recent",
        "/api/events",
        // Forge (GitHub/Gitea) PR viewer
        "/api/forge/config",
        "/api/forge/prs",
        "/api/forge/prs/{number}",
        "/api/forge/prs/{number}/impact",
        // Git analytics
        "/api/analytics/hotspots",
        "/api/analytics/deadcode",
    ];

    /// Extract every YAML key under `paths:` whose value starts with
    /// `/api/`. Cheap regex-free parser — assumes the canonical
    /// 2-space indent the file is written in.
    fn paths_from_openapi_yaml() -> std::collections::BTreeSet<String> {
        let mut in_paths = false;
        let mut out = std::collections::BTreeSet::new();
        for raw in OPENAPI_YAML.lines() {
            if !in_paths {
                if raw.trim_start() == "paths:" {
                    in_paths = true;
                }
                continue;
            }
            // A sibling top-level key (e.g. `components:`) ends the
            // section. Top-level keys have zero leading whitespace.
            if !raw.starts_with(' ') && !raw.starts_with('#') && !raw.trim().is_empty() {
                break;
            }
            // Path keys live at the 2-space indent: `  /api/foo:`.
            if let Some(rest) = raw.strip_prefix("  ") {
                if rest.starts_with('/') {
                    if let Some(p) = rest.split(':').next() {
                        out.insert(p.to_string());
                    }
                }
            }
        }
        out
    }

    #[test]
    fn openapi_yaml_lists_every_route() {
        let in_yaml = paths_from_openapi_yaml();
        let in_def: std::collections::BTreeSet<String> =
            KNOWN_API_PATHS.iter().map(|s| s.to_string()).collect();
        let missing_in_yaml: Vec<&String> = in_def.difference(&in_yaml).collect();
        let missing_in_def: Vec<&String> = in_yaml.difference(&in_def).collect();
        assert!(
            missing_in_yaml.is_empty() && missing_in_def.is_empty(),
            "OpenAPI spec drift detected.\n  \
             Routes missing from openapi.yaml: {missing_in_yaml:?}\n  \
             Paths in YAML but not in KNOWN_API_PATHS: {missing_in_def:?}\n  \
             Update both `crates/crabcc-viz/openapi.yaml` and \
             `KNOWN_API_PATHS` in `crates/crabcc-viz/src/lib.rs` so they \
             agree, then re-run `bun run codegen` from the web/ dir."
        );
    }

    #[test]
    fn openapi_yaml_is_non_empty() {
        // Smoke — guards against a file with the wrong path resolution.
        assert!(OPENAPI_YAML.contains("openapi: 3.1.0"));
        assert!(OPENAPI_YAML.contains("/api/bootstrap"));
    }

    #[test]
    fn parse_query_defaults() {
        let q = parse_query("root=Foo").unwrap();
        assert_eq!(q.root, "Foo");
        assert_eq!(q.dir, "callers");
        assert_eq!(q.depth, 2);
    }

    #[test]
    fn parse_query_callees_with_depth() {
        let q = parse_query("root=Bar&dir=callees&depth=4").unwrap();
        assert_eq!(q.root, "Bar");
        assert_eq!(q.dir, "callees");
        assert_eq!(q.depth, 4);
    }

    #[test]
    fn parse_query_rejects_bad_dir() {
        assert!(parse_query("root=X&dir=sideways").is_err());
    }

    #[test]
    fn parse_query_requires_root() {
        assert!(parse_query("dir=callers").is_err());
        assert!(parse_query("root=").is_err());
    }

    #[test]
    fn url_decode_handles_percent_and_plus() {
        assert_eq!(url_decode("foo%20bar"), "foo bar");
        assert_eq!(url_decode("foo+bar"), "foo bar");
        assert_eq!(url_decode("Mod%3A%3Afn"), "Mod::fn");
    }

    // =====================================================================
    // Telemetry tail tests — issue #90 dashboard surface.
    //
    // CI note: every `#[test]` in this block is `#[ignore]`'d per the
    // \"skip tracing-related work in main + PR CI\" directive (see
    // CI history around 2026-04-30). They exercise the
    // `.crabcc/telemetry.jsonl` parsing path, which is hot for the
    // /live dashboard but not load-bearing for any merge gate.
    // Run locally with `cargo test -p crabcc-viz -- --ignored telemetry_tail`
    // when touching `telemetry_tail` / `parse_iso8601_unix` /
    // `OtlpHealthSnapshot`. To re-enable in CI, drop the `#[ignore]`s
    // here AND verify rotel/OTLP isn't needed for them (they don't
    // talk to a collector — just parse local jsonl, so they're CI-safe;
    // ignore is a policy choice, not a technical requirement).
    // =====================================================================

    #[test]
    #[ignore = "tracing/telemetry path; skipped in CI per merge-policy directive"]
    fn parse_iso8601_unix_known_values() {
        // 1970-01-01T00:00:00Z = 0
        assert_eq!(super::parse_iso8601_unix("1970-01-01T00:00:00Z"), 0);
        // 2026-04-30T08:36:43Z. Hand-computed via `date -u -j -f ...`
        // is 1777538203 — must round-trip exactly (drops sub-second).
        assert_eq!(
            super::parse_iso8601_unix("2026-04-30T08:36:43Z"),
            1777538203
        );
        // Sub-second precision is dropped intentionally.
        assert_eq!(
            super::parse_iso8601_unix("2026-04-30T08:36:43.674476Z"),
            1777538203
        );
    }

    #[test]
    #[ignore = "tracing/telemetry path; skipped in CI per merge-policy directive"]
    fn telemetry_tail_missing_file_returns_empty_with_source_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let snap = super::telemetry_tail(dir.path(), "").unwrap();
        assert!(snap.events.is_empty());
        assert!(!snap.source.exists);
        assert_eq!(snap.source.lines_read, 0);
        assert!(snap.source.path.ends_with(".crabcc/telemetry.jsonl"));
    }

    #[test]
    #[ignore = "tracing/telemetry path; skipped in CI per merge-policy directive"]
    fn telemetry_tail_parses_jsonl_and_returns_events() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".crabcc")).unwrap();
        let body = concat!(
            r#"{"timestamp":"2026-04-30T08:36:43.674476Z","level":"INFO","fields":{"message":"graph build done","kpi":"graph.build","edges":3,"nodes":5,"duration_ms":0},"target":"crabcc_core::graph"}"#,
            "\n",
            r#"{"timestamp":"2026-04-30T08:36:44.000000Z","level":"INFO","fields":{"message":"graph cycles done","kpi":"graph.cycles","count":1,"duration_ms":0},"target":"crabcc_core::graph"}"#,
            "\n",
        );
        std::fs::write(dir.path().join(".crabcc/telemetry.jsonl"), body).unwrap();
        let snap = super::telemetry_tail(dir.path(), "").unwrap();
        assert_eq!(snap.events.len(), 2);
        assert!(snap.source.exists);
        assert_eq!(snap.source.lines_read, 2);
        // Events sorted by ts ascending; cursor = max ts.
        assert_eq!(snap.events[0].fields["kpi"], "graph.build");
        assert_eq!(snap.events[1].fields["kpi"], "graph.cycles");
        assert_eq!(snap.cursor, snap.events[1].ts);
    }

    #[test]
    #[ignore = "tracing/telemetry path; skipped in CI per merge-policy directive"]
    fn telemetry_tail_filters_by_since() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".crabcc")).unwrap();
        // Two events; since cuts off the first one.
        let body = concat!(
            r#"{"timestamp":"2026-04-30T08:36:43.674476Z","level":"INFO","fields":{"kpi":"graph.build"},"target":"x"}"#,
            "\n",
            r#"{"timestamp":"2026-04-30T08:36:44.000000Z","level":"INFO","fields":{"kpi":"graph.cycles"},"target":"x"}"#,
            "\n",
        );
        std::fs::write(dir.path().join(".crabcc/telemetry.jsonl"), body).unwrap();
        // 1777538203 = exact ts of event 1; since=1777538204 keeps only event 2.
        let snap = super::telemetry_tail(dir.path(), "since=1777538204").unwrap();
        assert_eq!(snap.events.len(), 1);
        assert_eq!(snap.events[0].fields["kpi"], "graph.cycles");
    }

    #[test]
    #[ignore = "tracing/telemetry path; skipped in CI per merge-policy directive"]
    fn telemetry_tail_tolerates_corrupt_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".crabcc")).unwrap();
        // Mix of valid + invalid lines. Bad lines are skipped, not raised.
        let body = concat!(
            "this is not json\n",
            r#"{"timestamp":"2026-04-30T08:36:43Z","level":"INFO","fields":{"kpi":"x"},"target":"t"}"#,
            "\n",
            "{ also: bad\n",
        );
        std::fs::write(dir.path().join(".crabcc/telemetry.jsonl"), body).unwrap();
        let snap = super::telemetry_tail(dir.path(), "").unwrap();
        assert_eq!(snap.events.len(), 1);
        // lines_read counts every non-empty line attempted.
        assert_eq!(snap.source.lines_read, 3);
    }

    #[test]
    #[ignore = "tracing/telemetry path; skipped in CI per merge-policy directive"]
    fn telemetry_tail_respects_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".crabcc")).unwrap();
        let mut body = String::new();
        for i in 0..50 {
            // Vary the timestamp seconds so events are deduplicated by ts.
            let s = format!("{:02}", i % 60);
            body.push_str(&format!(
                r#"{{"timestamp":"2026-04-30T08:36:{s}.000000Z","level":"INFO","fields":{{"kpi":"x","i":{i}}},"target":"t"}}"#
            ));
            body.push('\n');
        }
        std::fs::write(dir.path().join(".crabcc/telemetry.jsonl"), body).unwrap();
        let snap = super::telemetry_tail(dir.path(), "limit=5").unwrap();
        assert_eq!(snap.events.len(), 5);
        // Events are ts-ascending; tail kept the most recent.
        assert_eq!(snap.events.last().unwrap().fields["i"], 49);
    }
}
