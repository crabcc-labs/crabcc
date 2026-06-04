//! stdio + HTTP transports for the MCP server.
//!
//! All transports converge on `super::dispatch::handle_with` for actual
//! JSON-RPC routing — these modules only deal with framing (newline-
//! delimited JSON over stdio, HTTP request/response shapes, bearer
//! auth, response serialisation).

use crate::dispatch::handle_with;
use crate::error_response;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::Path;
use tiny_http::{Header, Method, Request, Response, Server};

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
        // serde_json::from_slice tolerates leading whitespace per RFC
        // 7159, so the bytes-only frame check above is the only
        // pre-parse work needed.
        let req: Value = match serde_json::from_slice(&buf) {
            Ok(v) => v,
            Err(e) => {
                let resp = error_response(None, -32700, &format!("parse error: {e}"));
                serde_json::to_writer(&mut writer, &resp)?;
                writer.write_all(b"\n")?;
                writer.flush()?;
                continue;
            }
        };
        let resp = handle_with(&req, root, dev);
        // Spec: notifications get no response. Skip empty/Null
        // (notifications/initialized in particular).
        if resp.is_null() {
            continue;
        }
        serde_json::to_writer(&mut writer, &resp)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
}

/// HTTP transport for the MCP server (#204 phase 1).
///
/// Exposes the same JSON-RPC dispatch as [`serve_io`] / [`serve_stdio_with`]
/// behind two endpoints:
///   - `POST /mcp` — sync request → response. Body is a single JSON-RPC
///     2.0 request; response is the JSON-RPC reply (200) or
///     `204 No Content` for notifications.
///   - `GET /health` — liveness probe; returns
///     `{"status":"ok","transport":"http",...}`.
///
/// Auth: when `token` is `Some(t)`, every `POST /mcp` must carry
/// `Authorization: Bearer <t>`. When `None`, no auth (loopback / dev
/// mode). The token comparison is byte-equal — for #204 phase 1 this
/// is acceptable since loopback-only is the default bind.
///
/// Concurrency: tiny_http's default thread pool serves each request,
/// so multiple in-flight MCP calls don't head-of-line each other —
/// matches the dashboard pattern in `crabcc-viz`.
///
/// SSE / streaming responses (e.g. `agent.run` progress events) land
/// in #204 phase 4. Phase 1 is sync-only.
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
            (&Method::Get, "/health") => {
                let body = json!({
                    "status": "ok",
                    "transport": "http",
                    "version": env!("CARGO_PKG_VERSION"),
                });
                let _ = respond_json(req, 200, body);
            }
            (&Method::Post, "/mcp") => {
                if !check_bearer(&req, token.as_deref()) {
                    let _ = req.respond(
                        Response::from_string(r#"{"error":"unauthorized"}"#)
                            .with_status_code(401)
                            .with_header(content_type_json()),
                    );
                    continue;
                }
                handle_post_mcp(req, root, dev);
            }
            _ => {
                let _ = req.respond(
                    Response::from_string(r#"{"error":"not found"}"#)
                        .with_status_code(404)
                        .with_header(content_type_json()),
                );
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
    // Constant-time comparison would be ideal; we accept naive ==
    // for phase 1 since loopback-only mitigates timing attacks. The
    // bot is the only intended caller and runs on the same host.
    got == format!("Bearer {expected}").as_str()
}

fn handle_post_mcp(mut req: Request, root: &Path, dev: bool) {
    let mut buf = String::new();
    if req.as_reader().read_to_string(&mut buf).is_err() {
        let _ = req.respond(
            Response::from_string(r#"{"error":"body read error"}"#)
                .with_status_code(400)
                .with_header(content_type_json()),
        );
        return;
    }
    let req_json: Value = match serde_json::from_str(&buf) {
        Ok(v) => v,
        Err(e) => {
            let err = error_response(None, -32700, &format!("parse error: {e}"));
            let _ = respond_json(req, 200, err);
            return;
        }
    };
    let resp = handle_with(&req_json, root, dev);
    if resp.is_null() {
        // JSON-RPC notification — no response body. 204 per HTTP
        // bridge convention so the client can distinguish from a
        // dropped reply.
        let _ = req.respond(Response::from_string("").with_status_code(204));
        return;
    }
    let _ = respond_json(req, 200, resp);
}

fn respond_json(req: Request, status: u16, body: Value) -> std::io::Result<()> {
    let body_str = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
    let resp = Response::from_string(body_str)
        .with_status_code(status as i32)
        .with_header(content_type_json());
    req.respond(resp)
}

fn content_type_json() -> Header {
    "Content-Type: application/json"
        .parse()
        .expect("static header")
}
