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

use anyhow::{Context, Result};
use crabcc_core::graph::{CallGraph, GraphHit};
use crabcc_core::store::Store;
use serde::Serialize;
use tiny_http::{Header, Method, Request, Response, Server};

const BUNDLED_INDEX: &str = include_str!("../assets/index.html");

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
}

impl Config {
    pub fn loopback(root: PathBuf, port: u16) -> Self {
        Self {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            root,
            no_open: true,
        }
    }
}

/// Boot the server and block until SIGINT (or `tiny_http` returns from
/// `incoming_requests`). Returns the bound `SocketAddr` only once the
/// server has shut down — for the smoke-test path where we need the
/// resolved port up-front, use `bind_listener` + `serve_with_listener`.
pub fn serve(cfg: Config) -> Result<()> {
    let listener = bind_listener(cfg.bind, cfg.port)?;
    let addr = listener.local_addr()?;
    print_banner(&cfg, addr);
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
fn print_banner(cfg: &Config, addr: SocketAddr) {
    let c = Style::for_stderr();
    let url = format!("http://{}:{}", addr.ip(), addr.port());
    let index_db = cfg.root.join(".crabcc").join("index.db");
    let graph_json = cfg.root.join(".crabcc").join("graph.json");

    let index_state = describe_path(&index_db);
    let graph_state = describe_path(&graph_json);

    let mut routes = String::new();
    routes.push_str(&format!("  {} {}/\n", c.dim("GET"), url));
    routes.push_str(&format!(
        "  {} {}/api/graph?root=NAME&dir=callers|callees&depth=N\n",
        c.dim("GET"),
        url
    ));
    routes.push_str(&format!(
        "  {} {}/api/activity?since=TS&limit=N\n",
        c.dim("GET"),
        url
    ));
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

    if method != Method::Get {
        return respond_status(request, 405, "method not allowed");
    }

    match path {
        "/" | "/index.html" => respond_html(request, BUNDLED_INDEX),
        "/api/health" => respond_json(request, &serde_json::json!({ "status": "ok" })),
        "/api/graph" => match graph_snapshot(root, query) {
            Ok(snapshot) => respond_json(request, &snapshot),
            Err(e) => respond_status(request, 400, &format!("bad request: {e}")),
        },
        "/api/activity" => match activity_tail(root, query) {
            Ok(activity) => respond_json(request, &activity),
            Err(e) => respond_status(request, 400, &format!("bad request: {e}")),
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
