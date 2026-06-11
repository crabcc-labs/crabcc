//! ### transport.rs — stdio + HTTP (+ SSE) transports
//!
//! **Skill:** [`skill/crabcc-mcp/SKILL.md`](../../skill/crabcc-mcp/SKILL.md)
//! **Dispatch:** [`crates/crabcc-mcp/src/dispatch.rs`](dispatch.rs)
//! **Mastodon:** [`crates/crabcc-mcp/src/mastodon.rs`](mastodon.rs)
//! **Dashboard:** [`crates/crabcc-mcp/src/dashboard.html`](dashboard.html)
//! **Bench:** [`crates/crabcc-mcp/benches/mastodon_transport.rs`](../benches/mastodon_transport.rs)
//!
//! ---
//!
//! All transports converge on `super::dispatch::handle_with` for actual
//! JSON-RPC routing — these modules only deal with framing (newline-
//! delimited JSON over stdio, HTTP request/response shapes, SSE
//! streaming, bearer auth, response serialisation).
//!
//! ### Internal modules
//!
//! - [`dispatch.rs`](dispatch.rs) — `handle_with()`, `dispatch_tool_inner()`
//! - [`mastodon.rs`](mastodon.rs) — Mastodon API tools + rate limiting + caching
//! - [`schema.rs`](schema.rs) — `tools_def_for()`, MCP tool schema builders
//! - [`memory.rs`](memory.rs) — `memory.*` tool dispatch
//! - [`crabcc-core`](../../crabcc-core/src/lib.rs) — `Store`, query engine, Fts, graph, upgrade
//! - [`dashboard.html`](dashboard.html) — embedded admin dashboard (served at GET /)
//!
//! ### Dependencies
//!
//! - `ureq` (blocking HTTP) — Mastodon API client
//! - `rmp-serde` (MessagePack) — binary wire format
//! - `flate2` (gzip) — response compression
//! - `sonic-rs` (SIMD JSON) — SSE4.2/AVX2 accelerated parsing
//! - `tiny_http` — HTTP/SSE server (sync, thread-per-request)
//!
//! ### Content negotiation
//!
//! The HTTP transport supports two wire formats, selected via standard
//! HTTP content negotiation headers:
//!
//! | Header | `application/json` | `application/msgpack` |
//! |---|---|---|
//! | **Content-Type** (request body) | JSON (serde_json) | MessagePack (rmp-serde) |
//! | **Accept** (response body) | JSON (serde_json) | MessagePack |
//!
//! When no headers are present, JSON is used (backward compatible).
//! SSE responses are always JSON-wrapped (SSE is a text protocol).
//!
//! MessagePack typically produces 30–50% smaller payloads than JSON
//! and deserializes faster, making it the preferred format for
//! high-throughput agent ↔ MCP communication.

use crate::dispatch::{handle_with, handle_with_session};
use crate::error_response;
use anyhow::Result;
use crabcc_core::store::Store;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tiny_http::{Header, Method, Request, Response, Server};
use tracing::{debug, info, warn};

/// Maximum request body size (16 MiB). Larger requests are rejected
/// with 413 Payload Too Large to prevent memory exhaustion.
pub(crate) const MAX_BODY_SIZE: usize = 16 * 1024 * 1024;

// ── common response headers ─────────────────────────────────────────

/// Add standard security + identification headers to every response.
fn add_common_headers<R: std::io::Read>(resp: Response<R>) -> Response<R> {
    resp.with_header(server_header())
        .with_header(x_content_type_header())
        .with_header(referrer_policy_header())
        .with_header(connection_keep_alive())
        .with_header(header_vary())
}

fn server_header() -> Header {
    format!("Server: crabcc-mcp/{}", env!("CARGO_PKG_VERSION"))
        .parse()
        .expect("static header")
}

fn x_content_type_header() -> Header {
    "X-Content-Type-Options: nosniff"
        .parse()
        .expect("static header")
}

fn referrer_policy_header() -> Header {
    "Referrer-Policy: no-referrer"
        .parse()
        .expect("static header")
}

fn connection_keep_alive() -> Header {
    "Connection: keep-alive".parse().expect("static header")
}

// ── wire format ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    MessagePack,
}

impl Format {
    fn content_type(&self) -> &'static str {
        match self {
            Format::Json => "application/json",
            Format::MessagePack => "application/msgpack",
        }
    }

    fn from_content_type(s: &str) -> Option<Format> {
        if s.contains("application/msgpack") || s.contains("application/x-msgpack") {
            Some(Format::MessagePack)
        } else if s.contains("application/json") {
            Some(Format::Json)
        } else {
            None
        }
    }
}

/// Detect the request body format from `Content-Type`.
fn detect_request_format(req: &Request) -> Format {
    req.headers()
        .iter()
        .find(|h| h.field.equiv("content-type"))
        .and_then(|h| Format::from_content_type(h.value.as_str()))
        .unwrap_or(Format::Json)
}

/// Detect the desired response format from `Accept`.
fn detect_response_format(req: &Request) -> Format {
    req.headers()
        .iter()
        .find(|h| h.field.equiv("accept"))
        .and_then(|h| {
            // Check for msgpack first (more specific), then json.
            let v = h.value.as_str();
            if v.contains("text/event-stream") {
                // SSE always uses JSON data framing — binary in SSE
                // would need base64, which defeats the purpose.
                return Some(Format::Json);
            }
            Format::from_content_type(v)
        })
        .unwrap_or(Format::Json)
}

/// Deserialize a request body into a `serde_json::Value`.
fn deserialize_body(body: &[u8], fmt: Format) -> Result<Value, String> {
    match fmt {
        Format::Json => {
            // sonic-rs SIMD fast path (SSE4.2/AVX2), fallback to serde_json
            sonic_rs::from_slice::<serde_json::Value>(body)
                .or_else(|_| serde_json::from_slice::<Value>(body))
                .map_err(|e| format!("json parse error: {e}"))
        }
        Format::MessagePack => {
            rmp_serde::from_slice::<Value>(body).map_err(|e| format!("msgpack parse error: {e}"))
        }
    }
}

/// Serialize a `serde_json::Value` to bytes in the given format.
fn serialize_body(value: &Value, fmt: Format) -> Result<Vec<u8>, String> {
    match fmt {
        Format::Json => {
            // sonic-rs SIMD fast path (SSE4.2/AVX2), fallback to serde_json
            sonic_rs::to_vec(value)
                .or_else(|_| serde_json::to_vec(value))
                .map_err(|e| format!("json serialize error: {e}"))
        }
        Format::MessagePack => {
            rmp_serde::to_vec(value).map_err(|e| format!("msgpack serialize error: {e}"))
        }
    }
}

// ── stdio transport ─────────────────────────────────────────────────

/// Serialize a JSON value to a writer.
/// Uses sonic-rs SIMD path (to_vec) with serde_json fallback, matching `deserialize_body`.
#[inline]
fn write_json<W: Write>(writer: &mut W, value: &Value) -> anyhow::Result<()> {
    let bytes = sonic_rs::to_vec(value).or_else(|_| serde_json::to_vec(value))?;
    writer.write_all(&bytes).map_err(Into::into)
}

/// Serve the MCP stdio transport. Callers pass the dev-mode flag
/// explicitly; resolve it from the env via [`crate::dev_mode_from_env`]
/// if you want the legacy `MCP_DEV=1` behaviour.
pub fn serve_stdio_with(root: &Path, dev: bool) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());
    let writer = stdout.lock();
    serve_io(reader, writer, root, dev)
}

/// Generic I/O variant — issue #89 slice 1.
///
/// Drives the JSON-RPC loop against any [`BufRead`] / [`Write`] pair.
/// `serve_stdio_with` wraps locked stdin/stdout; tests pipe a
/// `Cursor<Vec<u8>>` of newline-delimited JSON in and capture the
/// response stream on a `Vec<u8>` writer — no `tempfile`, no pipe,
/// no subprocess.
///
/// # Hot-path discipline
///
/// Three changes vs the obvious `read_line` / `writeln!("{}")` form:
///
/// 1. **`read_until(b'\n')` + `from_slice`** — skips the UTF-8
///    validation pass `read_line` does on every byte. serde_json's
///    parser does its own UTF-8 check on the strings it cares about,
///    so the upfront pass is duplicate work.
/// 2. **`to_writer` + `write_all(b"\n")`** — replaces
///    `writeln!(writer, "{value}")`, which goes through `Display` →
///    `Value::to_string()` and allocates an intermediate `String`
///    per response. The new form serialises directly into the
///    writer's buffer.
/// 3. **One reusable `Vec<u8>`** — `clear()` keeps the capacity, so
///    subsequent requests don't re-allocate after the first big-ish
///    one. Pre-sized 4 KiB to cover the common case (most MCP
///    requests fit in one TCP segment).
///
/// Net effect: zero `String` allocations on the steady-state path
/// (notifications + responses both); one `Vec<u8>` grow at most.
pub fn serve_io<R, W>(mut reader: R, mut writer: W, root: &Path, dev: bool) -> Result<()>
where
    R: BufRead,
    W: Write,
{
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    // One Store for the whole stdio session, reused across tool calls instead
    // of re-opening SQLite per request (that open was the per-call latency floor).
    let mut store: Option<Store> = None;
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
        // Skip empty / whitespace-only frames without going through
        // String::trim — bytes-only check, no UTF-8 validation.
        if buf.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        // sonic-rs SIMD fast path (SSE4.2/AVX2) with serde_json fallback,
        // matching the HTTP transport's deserialize_body pattern.
        let req: Value = match sonic_rs::from_slice::<Value>(&buf)
            .or_else(|_| serde_json::from_slice::<Value>(&buf))
        {
            Ok(v) => v,
            Err(e) => {
                let resp = error_response(None, -32700, &format!("parse error: {e}"));
                write_json(&mut writer, &resp)?;
                writer.write_all(b"\n")?;
                writer.flush()?;
                continue;
            }
        };
        let resp = handle_with_session(&req, root, dev, &mut store);
        // Spec: notifications get no response. Skip empty/Null
        // (notifications/initialized in particular).
        if resp.is_null() {
            continue;
        }
        write_json(&mut writer, &resp)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
}

// ── HTTP (+ SSE + content negotiation) transport ────────────────────

/// HTTP transport for the MCP server (sync + SSE + content negotiation).
///
/// Exposes the same JSON-RPC dispatch as [`serve_io`] / [`serve_stdio_with`]
/// behind three endpoints:
///   - `POST /mcp` — JSON-RPC 2.0 request → response. Format selected via
///     `Content-Type` (request) and `Accept` (response): `application/json`
///     (default, sonic-rs accelerated) or `application/msgpack`. SSE-wrapped
///     when `Accept: text/event-stream` is set (JSON data only).
///     Notifications: 204 sync / 202 SSE.
///   - `GET /health` — liveness probe.
///   - `GET /sse` — MCP SSE negotiation endpoint (2024-11-05).
///
/// Auth: when `token` is `Some(t)`, every `POST /mcp` and `GET /sse`
/// must carry `Authorization: Bearer <t>`.
///
/// Concurrency: tiny_http's default thread pool serves each request,
/// so multiple in-flight calls don't head-of-line each other.
pub fn serve_http(addr: SocketAddr, root: &Path, dev: bool, token: Option<String>) -> Result<()> {
    let server = Server::http(addr).map_err(|e| anyhow::anyhow!("bind {addr}: {e}"))?;
    let bound = server.server_addr();
    tracing::info!(
        target: "crabcc_mcp::http",
        addr = %bound,
        dev,
        auth = token.is_some(),
        "serve_http: listening"
    );

    for req in server.incoming_requests() {
        match (req.method(), req.url()) {
            (&Method::Get, "/") | (&Method::Get, "/dashboard") => {
                let html = include_str!("dashboard.html");
                let want_gzip = accepts_gzip(&req);
                let payload = if want_gzip {
                    compress_gzip(html.as_bytes())
                } else {
                    html.as_bytes().to_vec()
                };
                let payload_len = payload.len();
                let is_compressed = want_gzip && payload_len < html.len();
                let mut resp = add_common_headers(
                    Response::from_data(payload)
                        .with_status_code(200)
                        .with_header(html_content_type())
                        .with_header(cache_short()),
                );
                if is_compressed {
                    resp = resp.with_header(content_encoding_gzip());
                }
                let _ = req.respond(resp);
            }
            (&Method::Get, "/stats") => {
                let body = crate::mastodon::gather_stats();
                let body_bytes = serde_json::to_vec(&body).unwrap_or_default();
                let raw_len = body_bytes.len();
                let want_gzip = accepts_gzip(&req);
                let payload = if want_gzip {
                    compress_gzip(&body_bytes)
                } else {
                    body_bytes
                };
                let payload_len = payload.len();
                let is_compressed = want_gzip && payload_len < raw_len;
                let mut resp = add_common_headers(
                    Response::from_data(payload)
                        .with_status_code(200)
                        .with_header(content_type_json())
                        .with_header(cache_no_cache()),
                );
                if is_compressed {
                    resp = resp.with_header(content_encoding_gzip());
                }
                let _ = req.respond(resp);
            }
            (&Method::Get, "/health") => {
                let body = json!({
                    "status": "ok",
                    "transport": "http",
                    "sse": true,
                    "gzip": true,
                    "formats": ["json", "msgpack"],
                    "version": env!("CARGO_PKG_VERSION"),
                });
                let body_bytes = serde_json::to_vec(&body).unwrap_or_default();
                let raw_len = body_bytes.len();
                let want_gzip = accepts_gzip(&req);
                let payload = if want_gzip {
                    compress_gzip(&body_bytes)
                } else {
                    body_bytes
                };
                let payload_len = payload.len();
                let is_compressed = want_gzip && payload_len < raw_len;
                let mut resp = add_common_headers(
                    Response::from_data(payload)
                        .with_status_code(200)
                        .with_header(content_type_json())
                        .with_header(cache_short()),
                );
                if is_compressed {
                    resp = resp.with_header(content_encoding_gzip());
                }
                let _ = req.respond(resp);
            }
            (&Method::Get, "/sse") => {
                if !check_bearer(&req, token.as_deref()) {
                    let _ = req.respond(add_common_headers(
                        Response::from_string(r#"{"error":"unauthorized"}"#)
                            .with_status_code(401)
                            .with_header(content_type_json()),
                    ));
                    continue;
                }
                info!(target: "crabcc_mcp::http", "GET /sse — SSE negotiation");
                handle_get_sse(req);
            }
            (&Method::Post, "/mcp") => {
                if !check_bearer(&req, token.as_deref()) {
                    let _ = req.respond(add_common_headers(
                        Response::from_string(r#"{"error":"unauthorized"}"#)
                            .with_status_code(401)
                            .with_header(content_type_json()),
                    ));
                    continue;
                }
                let req_fmt = detect_request_format(&req);
                let resp_fmt = detect_response_format(&req);
                let want_sse = accepts_sse(&req);
                let span = tracing::span!(
                    tracing::Level::INFO,
                    "mcp_request",
                    req_fmt = ?req_fmt,
                    resp_fmt = ?resp_fmt,
                    sse = want_sse,
                );
                let _enter = span.enter();
                handle_post_mcp(req, root, dev, req_fmt, resp_fmt, want_sse);
            }
            _ => {
                let _ = req.respond(add_common_headers(
                    Response::from_string(r#"{"error":"not found"}"#)
                        .with_status_code(404)
                        .with_header(content_type_json()),
                ));
            }
        }
    }
    Ok(())
}

fn check_bearer(req: &Request, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    let got = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("authorization"))
        .map(|h| h.value.as_str())
        .unwrap_or_default();
    got == format!("Bearer {expected}").as_str()
}

fn handle_post_mcp(
    mut req: Request,
    root: &Path,
    dev: bool,
    req_fmt: Format,
    resp_fmt: Format,
    want_sse: bool,
) {
    // Read the raw body (may be JSON or MessagePack bytes)
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    if req.as_reader().read_to_end(&mut buf).is_err() {
        respond_error(req, 400, "body read error", resp_fmt, want_sse);
        return;
    }
    if buf.len() > MAX_BODY_SIZE {
        warn!(
            target: "crabcc_mcp::http",
            size = buf.len(),
            limit = MAX_BODY_SIZE,
            "request body too large"
        );
        respond_error(req, 413, "request body too large", resp_fmt, want_sse);
        return;
    }

    let req_json: Value = match deserialize_body(&buf, req_fmt) {
        Ok(v) => v,
        Err(e) => {
            let err = error_response(None, -32700, &e);
            if want_sse {
                let id = next_sse_id();
                let body = sse_event_with_id(
                    "error",
                    &serde_json::to_string(&err).unwrap_or_default(),
                    id,
                );
                let _ = respond_sse(req, 200, &body);
            } else {
                respond_value(req, 200, &err, resp_fmt);
            }
            return;
        }
    };

    let resp = handle_with(&req_json, root, dev);
    if resp.is_null() {
        // Notification: 204 sync, 202 SSE per MCP streamable-HTTP spec.
        if want_sse {
            let _ = req.respond(Response::empty(202));
        } else {
            let _ = req.respond(Response::from_string("").with_status_code(204));
        }
        return;
    }

    if want_sse {
        // SSE always uses JSON data framing (text protocol).
        let id = next_sse_id();
        let body = sse_event_with_id(
            "message",
            &serde_json::to_string(&resp).unwrap_or_default(),
            id,
        );
        let _ = respond_sse(req, 200, &body);
    } else {
        respond_value(req, 200, &resp, resp_fmt);
    }
}

/// Handle `GET /sse` — MCP SSE negotiation endpoint.
fn handle_get_sse(req: Request) {
    let host = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("host"))
        .map(|h| h.value.as_str())
        .unwrap_or("localhost");
    let endpoint_url = format!("http://{host}/mcp");

    let id = next_sse_id();
    // Send endpoint event + keep-alive heartbeats.
    // true streaming (connection held open with periodic heartbeats)
    // needs an async server (axum/hyper). For the sync tiny_http
    // transport, we send the endpoint + initial heartbeats and close.
    // Clients should reconnect periodically via Last-Event-Id.
    let body_str = format!(
        "id: {id}\nevent: endpoint\ndata: {endpoint_url}\n\n\
         : keep-alive — reconnect with Last-Event-Id: {id} to resume\n\n"
    );
    let _ = respond_sse(req, 200, &body_str);

    // For true keep-alive streaming (connection held open, heartbeats
    // every 30s, server→client push), use the async path:
    //   `crabcc serve --transport async` (future: hyper-based).
    // The sync path is suitable for polling clients that reconnect.
}

// ── SSE helpers ─────────────────────────────────────────────────────

fn accepts_sse(req: &Request) -> bool {
    req.headers()
        .iter()
        .any(|h| h.field.equiv("accept") && h.value.as_str().contains("text/event-stream"))
}

fn respond_sse(req: Request, status: u16, body: &str) -> std::io::Result<()> {
    let body_bytes = body.as_bytes();
    let want_gzip = accepts_gzip(&req);
    let payload = if want_gzip {
        compress_gzip(body_bytes)
    } else {
        body_bytes.to_vec()
    };
    let payload_len = payload.len();

    let mut resp = add_common_headers(
        Response::from_data(payload)
            .with_status_code(status as i32)
            .with_header(content_type_sse())
            .with_header(cache_no_cache()),
    );

    if want_gzip && payload_len < body_bytes.len() {
        resp = resp.with_header(content_encoding_gzip());
    }

    req.respond(resp)
}

fn content_type_sse() -> Header {
    "Content-Type: text/event-stream"
        .parse()
        .expect("static header")
}

fn cache_no_cache() -> Header {
    "Cache-Control: no-cache".parse().expect("static header")
}

fn cache_short() -> Header {
    "Cache-Control: public, max-age=60"
        .parse()
        .expect("static header")
}

fn header_vary() -> Header {
    "Vary: Accept, Accept-Encoding, Content-Type"
        .parse()
        .expect("static header")
}

// ── gzip ─────────────────────────────────────────────────────────────

/// Check whether the client accepts gzip encoding.
fn accepts_gzip(req: &Request) -> bool {
    req.headers()
        .iter()
        .any(|h| h.field.equiv("accept-encoding") && h.value.as_str().contains("gzip"))
}

/// gzip-compress a byte buffer. Returns the compressed bytes or the
/// original if compression fails / isn't worth it (tiny payloads).
#[inline]
fn compress_gzip(data: &[u8]) -> Vec<u8> {
    // Skip compression for tiny payloads — gzip overhead (~20 bytes)
    // can make the output larger than the input.
    if data.len() < 128 {
        return data.to_vec();
    }
    let mut e = GzEncoder::new(Vec::with_capacity(data.len() / 2), Compression::fast());
    if e.write_all(data).is_err() {
        return data.to_vec();
    }
    e.finish().unwrap_or_else(|_| data.to_vec())
}

// ── SSE event id ─────────────────────────────────────────────────────

/// Monotonic SSE event ID counter. Included in `id:` fields so clients
/// can resume via `Last-Event-Id` on reconnect.
static SSE_EVENT_ID: AtomicU64 = AtomicU64::new(1);

fn next_sse_id() -> u64 {
    SSE_EVENT_ID.fetch_add(1, Ordering::Relaxed)
}

#[inline]
pub(crate) fn sse_event_with_id(event: &str, data: &str, id: u64) -> String {
    // Pre-allocated: id (up to 20 digits) + "id: \nevent: \ndata: \n\n" + event + data
    let cap = 40 + event.len() + data.len();
    let mut s = String::with_capacity(cap);
    use std::fmt::Write;
    let _ = write!(s, "id: {id}\nevent: {event}\ndata: {data}\n\n");
    s
}

// ── response helpers ────────────────────────────────────────────────

/// Serialize a `serde_json::Value` and respond in the given format.
/// Applies gzip compression when the client sends `Accept-Encoding: gzip`
/// and the payload is large enough to benefit.
fn respond_value(req: Request, status: u16, value: &Value, fmt: Format) {
    let raw = match serialize_body(value, fmt) {
        Ok(b) => b,
        Err(e) => {
            let fallback = json!({"error": format!("serialization error: {e}")});
            respond_json(req, 500, fallback);
            return;
        }
    };

    let raw_len = raw.len();
    let want_gzip = accepts_gzip(&req);
    let body = if want_gzip { compress_gzip(&raw) } else { raw };
    let body_len = body.len();
    let is_compressed = want_gzip && body_len < raw_len;

    let mut resp = add_common_headers(
        Response::from_data(body)
            .with_status_code(status as i32)
            .with_header(content_type_for(fmt)),
    );

    if is_compressed {
        debug!(
            target: "crabcc_mcp::http",
            raw_len,
            gzip_len = body_len,
            ratio = raw_len as f64 / body_len as f64,
            "gzip compressed response"
        );
        resp = resp.with_header(content_encoding_gzip());
    }

    let _ = req.respond(resp);
}

fn content_encoding_gzip() -> Header {
    "Content-Encoding: gzip".parse().expect("static header")
}

fn respond_json(req: Request, status: u16, body: Value) {
    respond_value(req, status, &body, Format::Json);
}

fn content_type_for(fmt: Format) -> Header {
    let ct = fmt.content_type();
    format!("Content-Type: {ct}")
        .parse()
        .expect("static header")
}

fn content_type_json() -> Header {
    content_type_for(Format::Json)
}

fn html_content_type() -> Header {
    "Content-Type: text/html; charset=utf-8"
        .parse()
        .expect("static header")
}

/// Respond with an error in the requested format and framing.
fn respond_error(req: Request, status: u16, msg: &str, fmt: Format, want_sse: bool) {
    if want_sse {
        let id = next_sse_id();
        let err_json = serde_json::to_string(&json!({"error": msg})).unwrap_or_default();
        let body = sse_event_with_id("error", &err_json, id);
        let _ = respond_sse(req, status, &body);
    } else {
        let body = json!({"error": msg});
        respond_value(req, status, &body, fmt);
    }
}

// ── bench-accessible wrappers ───────────────────────────────────────

#[doc(hidden)]
pub fn compress_gzip_for_bench(data: &[u8]) -> Vec<u8> {
    compress_gzip(data)
}
