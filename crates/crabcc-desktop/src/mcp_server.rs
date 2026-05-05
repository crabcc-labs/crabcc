//! Minimal MCP server on a Unix domain socket.
//!
//! Listens for JSON-RPC 2.0 messages, newline-delimited, on a
//! `UnixListener`. Advertises the `sampling` capability during the
//! `initialize` handshake and routes inbound `sampling/createMessage`
//! requests to a [`SamplingHandler`]. Other methods return a typed
//! `method_not_found` error.
//!
//! Threading model:
//!   * One listener thread loops on `accept()`.
//!   * Each accepted connection gets its own daemon thread that
//!     reads JSON-RPC messages until EOF.
//!
//! Shutdown is best-effort: dropping [`McpServerHandle`] removes
//! the socket file. The listener and per-connection threads are
//! daemon-style — they die with the process. That's intentional
//! for the desktop app, where the MCP server's lifetime is the
//! app's lifetime.
//!
//! Spec target: `MCP-NATIVE.md` §4.1 (process model) and
//! `MCP-SAMPLING-OFFER.md` §3 (protocol surface).

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::sampling::{SamplingHandler, SamplingRequest};

/// MCP protocol version we advertise. MCP dates this with a
/// YYYY-MM-DD string. Bump when the upstream spec moves.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Resolved default socket path. Honours
/// `CRABCC_DESKTOP_MCP_SOCKET` first; falls back to
/// `$HOME/.crabcc/desktop/mcp.sock`. Returns `None` when neither
/// is determinable (no `$HOME`).
pub fn default_socket_path() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("CRABCC_DESKTOP_MCP_SOCKET") {
        if !s.is_empty() {
            return Some(PathBuf::from(s));
        }
    }
    let home = std::env::var("HOME").ok()?;
    if home.is_empty() {
        return None;
    }
    Some(
        PathBuf::from(home)
            .join(".crabcc")
            .join("desktop")
            .join("mcp.sock"),
    )
}

/// Owned by [`crate::state::AppState`]; keeps the socket file
/// around for the app's lifetime. Listener / connection threads
/// are daemons (die with the process), so this guard handles only
/// the unlink-on-drop.
pub struct McpServerHandle {
    socket_path: PathBuf,
}

impl McpServerHandle {
    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }
}

impl Drop for McpServerHandle {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl std::fmt::Debug for McpServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerHandle")
            .field("socket_path", &self.socket_path)
            .finish()
    }
}

/// Bind a Unix domain socket at `socket_path` and start the
/// listener thread. Subsequent connections route inbound
/// `sampling/createMessage` to `handler`.
///
/// Returns the lifecycle handle. Drop it to unlink the socket.
pub fn spawn(
    socket_path: PathBuf,
    handler: Arc<dyn SamplingHandler>,
) -> Result<McpServerHandle> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    // Stale socket from a previous unclean exit will block bind
    // with EADDRINUSE. Best-effort remove first.
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind socket {}", socket_path.display()))?;

    info!(
        target: "crabcc::mcp_server",
        path = %socket_path.display(),
        "MCP server listening",
    );

    let handler_clone = handler.clone();
    std::thread::Builder::new()
        .name("crabcc-mcp-listener".into())
        .spawn(move || listener_loop(listener, handler_clone))
        .context("spawn MCP listener thread")?;

    Ok(McpServerHandle { socket_path })
}

fn listener_loop(listener: UnixListener, handler: Arc<dyn SamplingHandler>) {
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                warn!(target: "crabcc::mcp_server", error = %e, "accept failed");
                continue;
            }
        };
        let h = handler.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("crabcc-mcp-conn".into())
            .spawn(move || {
                if let Err(e) = handle_connection(stream, h) {
                    debug!(target: "crabcc::mcp_server", error = %e, "connection ended");
                }
            })
        {
            warn!(target: "crabcc::mcp_server", error = %e, "spawn conn thread failed");
        }
    }
}

#[derive(Deserialize)]
struct JsonRpcMessage {
    #[serde(default)]
    jsonrpc: String,
    /// Absent on notifications. Present on requests + responses.
    #[serde(default)]
    id: Option<Value>,
    /// Absent on responses. Present on requests + notifications.
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

fn handle_connection(stream: UnixStream, handler: Arc<dyn SamplingHandler>) -> Result<()> {
    let read_stream = stream.try_clone().context("clone stream for reader")?;
    let mut reader = BufReader::new(read_stream);
    let mut writer = stream;
    let mut initialized = false;

    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .context("read JSON-RPC line")?;
        if n == 0 {
            return Ok(()); // EOF — peer closed the connection.
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let req: JsonRpcMessage = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                send_error(
                    &mut writer,
                    None,
                    -32700,
                    &format!("parse error: {e}"),
                )?;
                continue;
            }
        };

        if req.jsonrpc != "2.0" {
            send_error(&mut writer, req.id, -32600, "jsonrpc must be \"2.0\"")?;
            continue;
        }

        match req.method.as_str() {
            "initialize" => {
                let result = serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {
                        "sampling": {},
                    },
                    "serverInfo": {
                        "name": "crabcc-desktop",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                });
                send_result(&mut writer, req.id, result)?;
            }
            "notifications/initialized" => {
                // MCP notifications don't get a response.
                initialized = true;
            }
            // Cheap liveness probe — peers don't need to be
            // initialized to ping. Mirrors LSP.
            "ping" => {
                send_result(&mut writer, req.id, serde_json::json!({}))?;
            }
            "sampling/createMessage" => {
                if !initialized {
                    send_error(
                        &mut writer,
                        req.id,
                        -32002,
                        "server not initialized — send `initialize` first",
                    )?;
                    continue;
                }
                let params: SamplingRequest = match req
                    .params
                    .and_then(|p| serde_json::from_value(p).ok())
                {
                    Some(p) => p,
                    None => {
                        send_error(
                            &mut writer,
                            req.id,
                            -32602,
                            "invalid params for sampling/createMessage",
                        )?;
                        continue;
                    }
                };
                match handler.handle(params) {
                    Ok(resp) => {
                        let val = serde_json::to_value(&resp).unwrap_or(Value::Null);
                        send_result(&mut writer, req.id, val)?;
                    }
                    Err(e) => {
                        send_error(&mut writer, req.id, e.kind.code(), &e.message)?;
                    }
                }
            }
            other => {
                send_error(
                    &mut writer,
                    req.id,
                    -32601,
                    &format!("method not found: {other}"),
                )?;
            }
        }
    }
}

fn send_result(writer: &mut UnixStream, id: Option<Value>, result: Value) -> Result<()> {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    write_msg(writer, &msg)
}

fn send_error(
    writer: &mut UnixStream,
    id: Option<Value>,
    code: i32,
    message: &str,
) -> Result<()> {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    });
    write_msg(writer, &msg)
}

fn write_msg(writer: &mut UnixStream, msg: &Value) -> Result<()> {
    let mut s = serde_json::to_string(msg).context("serialize message")?;
    s.push('\n');
    writer
        .write_all(s.as_bytes())
        .context("write to socket")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampling::{
        Content, FinishReason, Role, SamplingError, SamplingHandler as SH, SamplingRequest,
        SamplingResponse,
    };

    /// Test handler that records the request and returns a fixed
    /// response. No network, no Ollama.
    struct EchoHandler {
        last: std::sync::Mutex<Option<SamplingRequest>>,
    }
    impl SH for EchoHandler {
        fn handle(&self, req: SamplingRequest) -> Result<SamplingResponse, SamplingError> {
            *self.last.lock().unwrap() = Some(req);
            Ok(SamplingResponse {
                role: Role::Assistant,
                content: Content::Text {
                    text: "echo".into(),
                },
                model: "test/echo".into(),
                stop_reason: FinishReason::EndTurn,
                usage: None,
            })
        }
    }

    fn make_server() -> (McpServerHandle, PathBuf, Arc<EchoHandler>) {
        // Use the system temp dir directly instead of `tempfile`
        // — Unix socket path length is capped at ~104 bytes on
        // macOS, and tempfile's default path can exceed that on
        // some hosts.
        let unique = format!(
            "crabcc-mcp-test-{}-{}.sock",
            std::process::id(),
            crate::inspector::CallEvent::next_id(),
        );
        let path = std::env::temp_dir().join(unique);
        let h = Arc::new(EchoHandler {
            last: std::sync::Mutex::new(None),
        });
        let server = spawn(path.clone(), h.clone()).expect("spawn server");
        (server, path, h)
    }

    fn send_recv(stream: &mut UnixStream, msg: &Value) -> Value {
        let mut s = msg.to_string();
        s.push('\n');
        stream.write_all(s.as_bytes()).unwrap();
        let read_stream = stream.try_clone().unwrap();
        let mut reader = BufReader::new(read_stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn send_notification(stream: &mut UnixStream, msg: &Value) {
        let mut s = msg.to_string();
        s.push('\n');
        stream.write_all(s.as_bytes()).unwrap();
        // Tiny pause so the server processes the notification
        // before we send the next request — otherwise the
        // sampling/createMessage that follows can race ahead and
        // see `initialized = false`.
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    /// One combined test for the env-var-vs-default precedence —
    /// can't split into two `#[test]`s because cargo runs them in
    /// parallel and `std::env::set_var` is process-wide, so a
    /// concurrent test mutating the same var races. Sequence the
    /// transitions inside one test instead.
    #[test]
    fn default_socket_path_precedence_env_then_home_fallback() {
        // Snapshot prior state so we don't trample whatever was
        // set when the test harness started.
        let prior = std::env::var("CRABCC_DESKTOP_MCP_SOCKET").ok();

        // 1. Env var beats default.
        std::env::set_var("CRABCC_DESKTOP_MCP_SOCKET", "/tmp/explicit.sock");
        assert_eq!(
            default_socket_path().unwrap(),
            PathBuf::from("/tmp/explicit.sock"),
        );

        // 2. Empty env var is treated as unset (opt-out idiom).
        std::env::set_var("CRABCC_DESKTOP_MCP_SOCKET", "");
        if let Ok(home) = std::env::var("HOME") {
            let p = default_socket_path().unwrap();
            assert!(p.starts_with(home));
            assert!(p.ends_with("desktop/mcp.sock"));
        }

        // 3. Var unset → HOME fallback.
        std::env::remove_var("CRABCC_DESKTOP_MCP_SOCKET");
        if let Ok(home) = std::env::var("HOME") {
            let p = default_socket_path().unwrap();
            assert!(p.starts_with(home));
        }

        // Restore prior value (if any) for any sibling tests that
        // may run after.
        match prior {
            Some(v) => std::env::set_var("CRABCC_DESKTOP_MCP_SOCKET", v),
            None => std::env::remove_var("CRABCC_DESKTOP_MCP_SOCKET"),
        }
    }

    #[test]
    fn initialize_advertises_sampling_capability() {
        let (_server, path, _h) = make_server();
        let mut stream = UnixStream::connect(&path).unwrap();
        let resp = send_recv(
            &mut stream,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {},
            }),
        );
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(resp["result"]["capabilities"]["sampling"].is_object());
        assert_eq!(resp["result"]["serverInfo"]["name"], "crabcc-desktop");
    }

    #[test]
    fn ping_does_not_require_initialized() {
        let (_server, path, _h) = make_server();
        let mut stream = UnixStream::connect(&path).unwrap();
        let resp = send_recv(
            &mut stream,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "ping",
            }),
        );
        assert!(resp["result"].is_object());
    }

    #[test]
    fn sampling_create_message_requires_initialized() {
        let (_server, path, _h) = make_server();
        let mut stream = UnixStream::connect(&path).unwrap();
        let resp = send_recv(
            &mut stream,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "sampling/createMessage",
                "params": {
                    "messages": [{
                        "role": "user",
                        "content": {"type": "text", "text": "hi"},
                    }],
                },
            }),
        );
        assert_eq!(resp["error"]["code"], -32002);
    }

    #[test]
    fn sampling_create_message_routes_to_handler_after_init() {
        let (_server, path, h) = make_server();
        let mut stream = UnixStream::connect(&path).unwrap();
        let _ = send_recv(
            &mut stream,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {},
            }),
        );
        send_notification(
            &mut stream,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
            }),
        );
        let resp = send_recv(
            &mut stream,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "sampling/createMessage",
                "params": {
                    "messages": [{
                        "role": "user",
                        "content": {"type": "text", "text": "hi"},
                    }],
                },
            }),
        );
        assert_eq!(resp["result"]["content"]["text"], "echo");
        assert_eq!(resp["result"]["model"], "test/echo");
        assert!(h.last.lock().unwrap().is_some());
    }

    #[test]
    fn unknown_method_returns_method_not_found() {
        let (_server, path, _h) = make_server();
        let mut stream = UnixStream::connect(&path).unwrap();
        let resp = send_recv(
            &mut stream,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
            }),
        );
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn parse_error_returns_negative_32700() {
        let (_server, path, _h) = make_server();
        let mut stream = UnixStream::connect(&path).unwrap();
        stream.write_all(b"{\"not valid json\n").unwrap();
        let read_stream = stream.try_clone().unwrap();
        let mut reader = BufReader::new(read_stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let resp: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(resp["error"]["code"], -32700);
    }

    #[test]
    fn missing_jsonrpc_field_returns_negative_32600() {
        let (_server, path, _h) = make_server();
        let mut stream = UnixStream::connect(&path).unwrap();
        let resp = send_recv(
            &mut stream,
            &serde_json::json!({
                "id": 1,
                "method": "ping",
            }),
        );
        assert_eq!(resp["error"]["code"], -32600);
    }

    #[test]
    fn drop_handle_unlinks_socket() {
        let (server, path, _h) = make_server();
        assert!(path.exists(), "socket file should exist while server is up");
        drop(server);
        // Allow the OS a moment to clean up.
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!path.exists(), "socket file should be unlinked on drop");
    }
}
