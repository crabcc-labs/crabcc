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

pub mod runtime;

use anyhow::{Context, Result};
use crabcc_core::graph::{CallGraph, GraphHit};
use crabcc_core::store::Store;
use serde::Serialize;
use tiny_http::{Header, Method, Request, Response, Server};

const BUNDLED_INDEX: &str = include_str!("../assets/index.html");
const BUNDLED_LIVE: &str = include_str!("../assets/live.html");

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

/// Multi-line startup banner showing version, bound URL, repo root, index
/// presence, and a few quick links. Goes to stderr so a piping invocation
/// like `crabcc serve --no-open 2>/dev/null` is silent. ANSI colors honor
/// `NO_COLOR` (https://no-color.org) and are stripped if stderr isn't a tty.
fn print_banner(cfg: &Config, addr: SocketAddr, init: Option<&runtime::InitOutcome>) {
    let c = Style::for_stderr();
    let url = format!("http://{}:{}", addr.ip(), addr.port());
    let index_db = cfg.root.join(".crabcc").join("index.db");
    let graph_json = cfg.root.join(".crabcc").join("graph.json");

    let index_state = describe_path(&index_db);
    let graph_state = describe_path(&graph_json);

    let mut routes = String::new();
    routes.push_str(&format!(
        "  {} {}/                         (interactive call-graph viewer)\n",
        c.dim("GET"),
        url
    ));
    routes.push_str(&format!(
        "  {} {}/live                     (live monitoring dashboard)\n",
        c.dim("GET"),
        url
    ));
    routes.push_str(&format!(
        "  {} {}/api/graph?root=&dir=&depth=\n",
        c.dim("GET"),
        url
    ));
    routes.push_str(&format!(
        "  {} {}/api/activity?since=TS&limit=N\n",
        c.dim("GET"),
        url
    ));
    routes.push_str(&format!(
        "  {} {}/api/memory/recent?since=TS&limit=N\n",
        c.dim("GET"),
        url
    ));
    routes.push_str(&format!("  {} {}/api/bootstrap\n", c.dim("GET"), url));
    routes.push_str(&format!("  {} {}/api/health\n", c.dim("GET"), url));

    eprintln!();
    eprintln!(
        "{}  {}",
        c.brand("crabcc viz"),
        c.dim(&format!("v{}", env!("CARGO_PKG_VERSION")))
    );
    eprintln!("{}", c.dim("─".repeat(54).as_str()));
    eprintln!("  {}    {}", c.label("listen"), c.bold(&url));
    eprintln!("  {}      {}", c.label("root"), cfg.root.display());
    eprintln!("  {}     {}", c.label("index"), index_state);
    eprintln!("  {}     {}", c.label("graph"), graph_state);
    eprintln!("  {}      {}", c.label("bind"), describe_bind(cfg.bind, &c));
    eprintln!(
        "  {}   {}",
        c.label("threads"),
        c.dim("tiny_http default pool")
    );
    if let Some(o) = init {
        let bits = format!(
            "{} files, {} symbols, {} graph edges, {} drawers",
            o.files, o.symbols, o.graph_edges, o.drawers
        );
        let action = if o.created_index {
            "indexed"
        } else {
            "refreshed"
        };
        eprintln!("  {}      {} ({bits})", c.label("init"), action);
    } else if !cfg.init {
        eprintln!(
            "  {}      {}",
            c.label("init"),
            c.dim("skipped (--no-init)")
        );
    }
    eprintln!();
    eprintln!("{}", c.dim("routes"));
    eprint!("{routes}");
    eprintln!();
    eprintln!("  {} {}", c.dim("→"), c.dim("Ctrl-C to stop"));
    eprintln!();
}

fn describe_path(p: &Path) -> String {
    match std::fs::metadata(p) {
        Ok(meta) => {
            let size = meta.len();
            let kb = size as f64 / 1024.0;
            let suffix = if kb >= 1024.0 {
                format!("{:.1} MB", kb / 1024.0)
            } else if kb >= 1.0 {
                format!("{kb:.1} KB")
            } else {
                format!("{size} B")
            };
            format!("{} ({})", p.display(), suffix)
        }
        Err(_) => format!(
            "{} (missing — run `crabcc index` and `crabcc graph build`)",
            p.display()
        ),
    }
}

fn describe_bind(ip: IpAddr, c: &Style) -> String {
    if ip.is_loopback() {
        format!("{} {}", ip, c.dim("(loopback only)"))
    } else {
        format!(
            "{} {}",
            ip,
            c.warn("(non-loopback — viewer is unauthenticated)")
        )
    }
}

/// Tiny ANSI helper that disables colors when `NO_COLOR` is set, when
/// `CRABCC_NO_COLOR` is set (project-specific override), or when stderr
/// is not a tty (e.g. redirected to a logfile). We don't pull in `nu-ansi`
/// or `colored` for this — half a dozen escape codes don't justify a dep.
struct Style {
    on: bool,
}

impl Style {
    fn for_stderr() -> Self {
        let no_color =
            std::env::var_os("NO_COLOR").is_some() || std::env::var_os("CRABCC_NO_COLOR").is_some();
        #[cfg(unix)]
        let is_tty = libc_isatty(2);
        #[cfg(not(unix))]
        let is_tty = true;
        Self {
            on: !no_color && is_tty,
        }
    }
    fn brand(&self, s: &str) -> String {
        self.wrap(s, "\x1b[1;38;5;208m")
    }
    fn label(&self, s: &str) -> String {
        self.wrap(s, "\x1b[38;5;244m")
    }
    fn dim(&self, s: &str) -> String {
        self.wrap(s, "\x1b[2m")
    }
    fn bold(&self, s: &str) -> String {
        self.wrap(s, "\x1b[1m")
    }
    fn warn(&self, s: &str) -> String {
        self.wrap(s, "\x1b[1;33m")
    }
    fn wrap(&self, s: &str, prefix: &str) -> String {
        if self.on {
            format!("{prefix}{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
}

#[cfg(unix)]
fn libc_isatty(fd: i32) -> bool {
    // SAFETY: `isatty` only inspects a file-descriptor table entry; no
    // pointer dereference, no aliasing concerns.
    unsafe extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    unsafe { isatty(fd) == 1 }
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
        "/" | "/index.html" => respond_html(request, BUNDLED_INDEX),
        "/live" => respond_html(request, BUNDLED_LIVE),
        "/api/health" => respond_json(request, &serde_json::json!({ "status": "ok" })),
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
        "/api/debug/dump" => match debug_dump(root) {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("dump failed: {e}")),
        },
        "/api/memory/recent" => match memory_recent(root, query) {
            Ok(snap) => respond_json(request, &snap),
            Err(e) => respond_status(request, 500, &format!("memory snapshot failed: {e}")),
        },
        _ => respond_status(request, 404, "not found"),
    }
}

#[derive(Serialize)]
struct GraphSnapshot {
    root: String,
    dir: String,
    depth: usize,
    truncated: bool,
    nodes: Vec<NodeOut>,
    edges: Vec<EdgeOut>,
}

#[derive(Serialize)]
struct NodeOut {
    id: String,
    depth: usize,
}

#[derive(Serialize)]
struct EdgeOut {
    src: String,
    dst: String,
}

/// Build a bounded BFS snapshot of the call graph for the given root symbol.
///
/// The raw `CallGraph::incoming` / `CallGraph::outgoing` return only the
/// frontier symbol names + their depths; the viewer additionally needs the
/// edges *between* those nodes so the canvas layout has something to render.
/// We materialize the induced subgraph here by walking each node's outgoing
/// (or incoming) adjacency and keeping only edges where both endpoints are
/// in the BFS frontier.
fn graph_snapshot(root: &Path, query: &str) -> Result<GraphSnapshot> {
    let q = parse_query(query)?;
    let depth = q.depth.min(MAX_DEPTH);

    // Open the SQLite store and the cached graph. We don't try to refresh
    // the index here — `crabcc serve` is a viewer, not an indexer; users
    // run `crabcc index` / `crabcc refresh` separately. (Phase 2 will push
    // a "stale index" notice over WebSocket when the on-disk db mtime moves.)
    let db = root.join(".crabcc").join("index.db");
    let store = Store::open(&db).with_context(|| format!("opening store at {}", db.display()))?;
    let graph_path = root.join(".crabcc").join("graph.json");
    let graph = if graph_path.exists() {
        CallGraph::load(&graph_path)?
    } else {
        CallGraph::build(&store, root)?
    };

    let dir = q.dir.as_str();
    let frontier: Vec<GraphHit> = match dir {
        "callees" => graph.outgoing(&q.root, depth),
        _ => graph.incoming(&q.root, depth),
    };

    // The frontier from `incoming` / `outgoing` excludes the root itself.
    // Add it back at depth 0 so the canvas has a recognizable focus point.
    let mut nodes: Vec<NodeOut> = std::iter::once(NodeOut {
        id: q.root.clone(),
        depth: 0,
    })
    .chain(frontier.into_iter().map(|h| NodeOut {
        id: h.name,
        depth: h.depth,
    }))
    .collect();
    let truncated = nodes.len() > MAX_NODES;
    if truncated {
        nodes.truncate(MAX_NODES);
    }

    let in_set: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let mut edges: Vec<EdgeOut> = Vec::with_capacity(nodes.len() * 2);
    for n in &nodes {
        // For a `callees` view we draw edges in the call direction
        // (root → callee), and for `callers` we draw caller → root. The
        // direction of the arrow visualizes "who calls whom" in both modes.
        if dir == "callees" {
            if let Some(neighbors) = graph.callees.get(&n.id) {
                for nb in neighbors {
                    if in_set.contains(nb.as_str()) {
                        edges.push(EdgeOut {
                            src: n.id.clone(),
                            dst: nb.clone(),
                        });
                    }
                }
            }
        } else if let Some(neighbors) = graph.callers.get(&n.id) {
            for nb in neighbors {
                if in_set.contains(nb.as_str()) {
                    edges.push(EdgeOut {
                        src: nb.clone(),
                        dst: n.id.clone(),
                    });
                }
            }
        }
    }

    Ok(GraphSnapshot {
        root: q.root,
        dir: q.dir,
        depth,
        truncated,
        nodes,
        edges,
    })
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
            "repo" => {
                if v == "*" {
                    repo_filter = None;
                }
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

// ── /api/bootstrap ──────────────────────────────────────────────────────
//
// One-shot "what does the live dashboard need to know on first paint?"
// snapshot. Combines repo metadata with index sidecar stats so the
// header section can render before we wait on /api/activity. Fast: a
// cold call against an indexed repo on this machine measures sub-50ms.

#[derive(Serialize)]
struct BootstrapSnapshot {
    repo: String,
    root: String,
    version: &'static str,
    index: IndexState,
    graph: GraphState,
    memory: MemoryState,
}

#[derive(Serialize)]
struct IndexState {
    present: bool,
    files: usize,
    symbols: usize,
    edges: usize,
    db_bytes: u64,
    db_mtime: u64,
}

#[derive(Serialize)]
struct GraphState {
    present: bool,
    edges: usize,
    callers: usize,
    callees: usize,
}

#[derive(Serialize)]
struct MemoryState {
    present: bool,
    drawers: usize,
}

fn bootstrap_snapshot(root: &Path) -> Result<BootstrapSnapshot> {
    let repo = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();
    let db_path = root.join(".crabcc").join("index.db");
    let graph_path = root.join(".crabcc").join("graph.json");
    let memory_path = root.join(".crabcc").join("memory.db");

    let mut index = IndexState {
        present: db_path.exists(),
        files: 0,
        symbols: 0,
        edges: 0,
        db_bytes: 0,
        db_mtime: 0,
    };
    if let Ok(meta) = std::fs::metadata(&db_path) {
        index.db_bytes = meta.len();
        index.db_mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
    }
    if index.present {
        // Open in read-only-ish fashion via Store — costs about a stat
        // plus three count(*) round-trips, all cheap on an indexed db.
        if let Ok(store) = crabcc_core::store::Store::open(&db_path) {
            index.files = store.list_files().map(|v| v.len()).unwrap_or(0);
            index.symbols = store.iter_all_symbols().map(|v| v.len()).unwrap_or(0);
            index.edges = store.edge_count().map(|n| n as usize).unwrap_or(0);
        }
    }

    let mut graph = GraphState {
        present: graph_path.exists(),
        edges: 0,
        callers: 0,
        callees: 0,
    };
    if graph.present {
        if let Ok(g) = crabcc_core::graph::CallGraph::load(&graph_path) {
            graph.edges = g.edge_count;
            graph.callers = g.callers.len();
            graph.callees = g.callees.len();
        }
    }

    let mut memory = MemoryState {
        present: memory_path.exists(),
        drawers: 0,
    };
    if memory.present {
        // Palace::open does its own bootstrap; we don't want a fresh
        // schema-create as a side effect of a viewer GET. Drop into the
        // raw rusqlite path used by the backend instead.
        if let Ok(conn) = rusqlite::Connection::open_with_flags(
            &memory_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            if let Ok(n) =
                conn.query_row("select count(*) from drawers", [], |r| r.get::<_, i64>(0))
            {
                memory.drawers = n as usize;
            }
        }
    }

    Ok(BootstrapSnapshot {
        repo,
        root: root.display().to_string(),
        version: env!("CARGO_PKG_VERSION"),
        index,
        graph,
        memory,
    })
}

// ── /api/memory/recent ──────────────────────────────────────────────────
//
// Returns the most-recently-created memory drawers for the live feed's
// "new entries" column. Uses raw SQL against the memory db (read-only
// flags) rather than `Palace::list_drawers` because we don't want the
// schema-bootstrap side effects of `Palace::open` on every poll.

#[derive(Serialize)]
struct MemoryRecentSnapshot {
    present: bool,
    cursor: i64,
    drawers: Vec<DrawerOut>,
}

#[derive(Serialize)]
struct DrawerOut {
    id: i64,
    wing: String,
    room: Option<String>,
    source_id: String,
    body_preview: String,
    created_at: i64,
}

fn memory_recent(root: &Path, query: &str) -> Result<MemoryRecentSnapshot> {
    let mut since: i64 = 0;
    let mut limit: usize = 20;
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = url_decode(v);
        match k {
            "since" => since = v.parse().unwrap_or(0),
            "limit" => limit = v.parse::<usize>().unwrap_or(20).clamp(1, 200),
            _ => {}
        }
    }
    let memory_path = root.join(".crabcc").join("memory.db");
    if !memory_path.exists() {
        return Ok(MemoryRecentSnapshot {
            present: false,
            cursor: since,
            drawers: vec![],
        });
    }
    let conn = rusqlite::Connection::open_with_flags(
        &memory_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    // The drawer body can be huge; preview only the first ~240 chars
    // for the live feed. Clients that want the full body call
    // `crabcc memory get <id>` (a separate, more expensive path).
    // The drawers schema uses FKs to `wings` + `rooms` (not flat columns),
    // so we LEFT JOIN to surface human-readable names. body_enc != 0
    // means FSST-compressed; we skip those rows in the preview because
    // decoding requires the codec from `~/.crabcc/fsst.symbols` and we
    // don't want the live feed to depend on optional sidecars. The
    // count line above already includes them, so the preview just
    // shows fewer rows than `count` when compression is on — that's
    // fine for a live dashboard.
    let mut stmt = conn.prepare(
        "SELECT d.id, w.name, r.name, d.source_id, substr(d.body, 1, 240), d.created_at \
         FROM drawers d \
         LEFT JOIN wings w ON w.id = d.wing_id \
         LEFT JOIN rooms r ON r.id = d.room_id \
         WHERE d.created_at > ?1 AND d.body_enc = 0 \
         ORDER BY d.created_at DESC \
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![since, limit as i64], |r| {
        Ok(DrawerOut {
            id: r.get(0)?,
            wing: r.get::<_, Option<String>>(1)?.unwrap_or_else(|| "?".into()),
            room: r.get::<_, Option<String>>(2)?,
            source_id: r.get(3)?,
            body_preview: r.get(4)?,
            created_at: r.get(5)?,
        })
    })?;
    let mut drawers: Vec<DrawerOut> = rows.filter_map(|r| r.ok()).collect();
    let cursor = drawers.iter().map(|d| d.created_at).max().unwrap_or(since);
    // Reverse so the JSON is oldest-first within the page; the
    // frontend prepends each event to its list which gives the user
    // the natural "newest at top" ordering after concatenation.
    drawers.reverse();
    Ok(MemoryRecentSnapshot {
        present: true,
        cursor,
        drawers,
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
        .map(|id| NodeOut {
            id: id.clone(),
            // Seeds are "depth 0" (queried-equivalent), neighbors are 1.
            depth: if seeds.contains(id) { 0 } else { 1 },
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

struct Query {
    root: String,
    dir: String,
    depth: usize,
}

fn parse_query(raw: &str) -> Result<Query> {
    let mut root = None;
    let mut dir = String::from("callers");
    let mut depth = 2usize;
    for pair in raw.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = match pair.split_once('=') {
            Some(kv) => kv,
            None => (pair, ""),
        };
        let v = url_decode(v);
        match k {
            "root" => root = Some(v),
            "dir" => {
                if v == "callers" || v == "callees" {
                    dir = v;
                } else {
                    anyhow::bail!("dir must be 'callers' or 'callees'");
                }
            }
            "depth" => {
                depth = v
                    .parse::<usize>()
                    .map_err(|_| anyhow::anyhow!("depth must be a non-negative integer"))?;
            }
            _ => {}
        }
    }
    let root = root.ok_or_else(|| anyhow::anyhow!("missing required parameter: root"))?;
    if root.is_empty() {
        anyhow::bail!("root must be non-empty");
    }
    Ok(Query { root, dir, depth })
}

/// Minimal percent-decoder for query-string values. We only accept ASCII
/// printable identifiers + a few separators here, so a hand-rolled decoder
/// avoids pulling in a urlencoding crate just for this one call site.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = &bytes[i + 1..i + 3];
                if let (Some(h), Some(l)) = (hex_digit(hex[0]), hex_digit(hex[1])) {
                    out.push((h << 4) | l);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
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
        let meta_path = p.join("meta.json");

        let status = if lock.exists() { "running" } else { "exited" };
        let pid = std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        let log_bytes = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);

        // meta.json is optional — older runs (or `--dry-run`) don't
        // always have it. Best-effort parse; missing fields fall to
        // sensible defaults so the UI never breaks on a half-written
        // run dir.
        let mut started_ts = 0u64;
        let mut runtime_label = String::from("subprocess (host)");
        let mut model: Option<String> = None;
        let mut prompt_preview = String::new();
        let mut root: Option<String> = None;
        if let Ok(body) = std::fs::read_to_string(&meta_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                started_ts = v["started_ts"].as_u64().unwrap_or(0);
                runtime_label = v["runtime"].as_str().unwrap_or("?").to_string();
                model = v["model"].as_str().map(|s| s.to_string());
                prompt_preview = v["prompt_preview"].as_str().unwrap_or("").to_string();
                root = v["root"].as_str().map(|s| s.to_string());
            }
        }
        agents.push(AgentSummary {
            id,
            started_ts,
            status,
            pid,
            runtime: runtime_label,
            model,
            prompt_preview,
            log_bytes,
            root,
        });
    }
    // Most recent first; the dashboard shows running runs at the top.
    agents.sort_by(|a, b| b.started_ts.cmp(&a.started_ts));
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
    // Parse JSON body: `{ "prompt": "...", "model"?: "...", "no_refresh"?: bool }`.
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
    }
    let req: LaunchReq = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => return respond_status(request, 400, &format!("invalid JSON: {e}")),
    };
    if req.prompt.trim().is_empty() {
        return respond_status(request, 400, "prompt must be non-empty");
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
    let pid = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    let is_alive = pid.map(pid_alive).unwrap_or(false);
    let log_bytes = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);

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

    Ok(AgentInfo {
        id: id.to_string(),
        status,
        pid,
        is_alive,
        started_ts,
        runtime: runtime_label,
        model,
        prompt_chars,
        prompt_preview,
        root,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
