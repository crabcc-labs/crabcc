//! Long-lived bridge mode. Speaks MCP JSON-RPC 2.0 on stdin/stdout to
//! an MCP client (e.g. Claude Code), accepts a single Chrome host
//! connection on a loopback TCP socket, and translates between them.
//!
//! Single-host design: only one Chrome host may be connected at a
//! time. A second connection is rejected with `{"kind":"error", ...}`
//! and immediately closed. This matches the threat model for native
//! messaging — one extension instance per browser profile per process.
//!
//! MCP layer (Phase 1):
//! - `initialize` → returns capabilities + protocol version
//! - `tools/list` → returns one tool per BridgeMethod / CapabilityMethod
//! - `tools/call` → forwards as RpcRequest, awaits RpcResponse, returns
//!   the result inside an MCP `content` array
//!
//! Anything richer (resources, prompts, sampling) is unimplemented and
//! returns the standard JSON-RPC `-32601 method not found`.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use crate::config;

/// All callable methods. Any tool name that doesn't appear here is
/// rejected before we hit the host. Keep in sync with the extension's
/// BridgeMethod / CapabilityMethod unions in `apps/crabcc-chrome-extension`.
const METHODS: &[&str] = &[
    "schema",
    "state",
    "buttons",
    "click",
    "waitFor",
    "perfMemory",
    "navigate",
    "goBack",
    "goForward",
    "pressKey",
    "hover",
    "type",
    "selectOption",
    "drag",
    "ariaSnapshot",
    "clickByRef",
    "hoverByRef",
    "typeByRef",
    "captureVisibleTab",
    "tabInfo",
    "debuggerAttach",
    "debuggerDetach",
    "debuggerEvaluate",
    "debuggerConsoleList",
    "debuggerConsoleClear",
    "debuggerNetworkList",
    "debuggerNetworkBody",
    "debuggerNetworkClear",
    "v8CollectGarbage",
    "v8HeapSnapshot",
    "v8ProfileStart",
    "v8ProfileStop",
    "v8Metrics",
];

pub fn run() -> Result<()> {
    let cfg = config::load_or_default();
    if cfg.secret.is_empty() {
        return Err(anyhow!(
            "no secret in chrome.toml — run `crabcc-chrome pair --id <ext-id>` first"
        ));
    }

    // Bind 127.0.0.1:0 so the OS picks a free ephemeral port. Persist
    // the port back to chrome.toml so the host can find us.
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .context("binding loopback listener")?;
    let port = listener.local_addr()?.port();
    let mut updated = cfg.clone();
    updated.port = port;
    config::save(&updated).context("persisting port")?;
    tracing::info!(port, "serve: bridge listening");

    // Shared "host connection" cell — written by the accept thread,
    // read by the dispatcher. The condvar wakes the dispatcher the
    // moment a host connects.
    let host_state = Arc::new((Mutex::new(HostState::default()), Condvar::new()));

    let host_state_a = host_state.clone();
    let secret = cfg.secret.clone();
    thread::spawn(move || {
        accept_loop(listener, host_state_a, secret);
    });

    // Stdin loop runs on the main thread so the process exits when the
    // MCP client disconnects (closing stdin → main returns).
    mcp_loop(host_state)
}

#[derive(Default)]
struct HostState {
    /// `Some` once a host has authenticated. Replaced on reconnect.
    write_half: Option<TcpStream>,
    /// Inbox of inbound JSON lines from the host (responses + events).
    /// Bounded — drops oldest on overflow.
    inbox: VecDeque<String>,
}

const MAX_INBOX: usize = 256;

fn accept_loop(
    listener: TcpListener,
    host_state: Arc<(Mutex<HostState>, Condvar)>,
    expected_secret: String,
) {
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "accept failure");
                continue;
            }
        };
        let host_state = host_state.clone();
        let secret = expected_secret.clone();
        thread::spawn(move || {
            if let Err(e) = handle_host(stream, host_state, secret) {
                tracing::warn!(error = %e, "host handler exited");
            }
        });
    }
}

fn handle_host(
    stream: TcpStream,
    host_state: Arc<(Mutex<HostState>, Condvar)>,
    expected_secret: String,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_nodelay(true).ok();
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    // Auth handshake: read one line, parse, validate.
    let mut auth_line = String::new();
    reader.read_line(&mut auth_line)?;
    let auth: AuthMsg = serde_json::from_str(auth_line.trim()).context("parsing auth")?;
    if auth.kind != "auth" || auth.secret != expected_secret {
        let _ = writeln!(
            writer,
            "{}",
            serde_json::json!({"kind":"error","message":"auth failed"})
        );
        return Err(anyhow!("host auth failed"));
    }
    if auth.wire_version != crate::WIRE_VERSION {
        let _ = writeln!(
            writer,
            "{}",
            serde_json::json!({
                "kind":"error",
                "message": format!("wireVersion mismatch — bridge expects {}, host sent {}", crate::WIRE_VERSION, auth.wire_version)
            })
        );
        return Err(anyhow!("wireVersion mismatch"));
    }

    {
        let (lock, cv) = &*host_state;
        let mut s = lock.lock().expect("host_state poisoned");
        s.write_half = Some(writer.try_clone()?);
        cv.notify_all();
    }
    tracing::info!("serve: host authenticated");

    // Push every inbound line into the shared inbox so the dispatcher
    // can correlate by RpcResponse.id.
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim_end_matches(&['\r', '\n'][..]).to_string();
                if trimmed.is_empty() {
                    continue;
                }
                let (lock, cv) = &*host_state;
                let mut s = lock.lock().expect("host_state poisoned");
                if s.inbox.len() >= MAX_INBOX {
                    s.inbox.pop_front();
                }
                s.inbox.push_back(trimmed);
                cv.notify_all();
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                continue;
            }
            Err(e) => {
                tracing::info!(error = %e, "host disconnected");
                break;
            }
        }
    }

    let (lock, cv) = &*host_state;
    let mut s = lock.lock().expect("host_state poisoned");
    s.write_half = None;
    cv.notify_all();
    Ok(())
}

#[derive(Debug, Deserialize)]
struct AuthMsg {
    kind: String,
    secret: String,
    #[serde(rename = "wireVersion")]
    wire_version: u32,
}

// --- MCP layer ------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse<T: Serialize> {
    jsonrpc: &'static str,
    id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

fn mcp_loop(host_state: Arc<(Mutex<HostState>, Condvar)>) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut next_rpc_id: u64 = 1;

    let mut line = String::new();
    loop {
        line.clear();
        let n = stdin.lock().read_line(&mut line)?;
        if n == 0 {
            return Ok(());
        }
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if trimmed.is_empty() {
            continue;
        }
        let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                emit_error(&stdout, serde_json::Value::Null, -32700, &e.to_string());
                continue;
            }
        };
        let id = req.id.clone().unwrap_or(serde_json::Value::Null);

        match req.method.as_str() {
            "initialize" => {
                emit_ok(
                    &stdout,
                    id,
                    serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "crabcc-chrome", "version": env!("CARGO_PKG_VERSION")}
                    }),
                );
            }
            "tools/list" => {
                let tools: Vec<_> = METHODS
                    .iter()
                    .map(|name| {
                        serde_json::json!({
                            "name": format!("browser_{name}"),
                            "description": tool_description(name),
                            // Permissive schema — translation happens in
                            // tools/call. Full per-tool input schemas
                            // duplicate the TS-side type defs and would
                            // drift; we just accept any object.
                            "inputSchema": {"type": "object", "additionalProperties": true}
                        })
                    })
                    .collect();
                emit_ok(&stdout, id, serde_json::json!({"tools": tools}));
            }
            "tools/call" => {
                let res = handle_tool_call(&host_state, &req.params, &mut next_rpc_id);
                match res {
                    Ok(value) => emit_ok(
                        &stdout,
                        id,
                        serde_json::json!({
                            "content": [{"type": "text", "text": value.to_string()}],
                            "isError": false,
                        }),
                    ),
                    Err(e) => emit_ok(
                        &stdout,
                        id,
                        serde_json::json!({
                            "content": [{"type": "text", "text": e.to_string()}],
                            "isError": true,
                        }),
                    ),
                }
            }
            _ => emit_error(
                &stdout,
                id,
                -32601,
                &format!("method not found: {}", req.method),
            ),
        }
    }
}

fn handle_tool_call(
    host_state: &Arc<(Mutex<HostState>, Condvar)>,
    params: &serde_json::Value,
    next_id: &mut u64,
) -> Result<serde_json::Value> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("tools/call: missing `name`"))?;
    let bridge_method = name
        .strip_prefix("browser_")
        .ok_or_else(|| anyhow!("tool name must start with `browser_`"))?;
    if !METHODS.contains(&bridge_method) {
        return Err(anyhow!("unknown tool: {name}"));
    }
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    // The extension's RpcRequest uses positional `args: any[]`. MCP
    // tool-call arguments are a free-form object. We ship the object
    // as the single positional arg, which matches how every existing
    // BridgeMethod is shaped (the methods that take multiple args
    // accept an `opts` trailing object so positional vs keyed agree).
    let rpc_id = *next_id;
    *next_id = next_id.wrapping_add(1);
    let envelope = serde_json::json!({
        "id": rpc_id,
        "method": bridge_method,
        "args": [args],
    });
    let response = send_and_wait(host_state, envelope, rpc_id, Duration::from_secs(30))?;
    if response.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return Err(anyhow!(response
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string()));
    }
    Ok(response
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

fn send_and_wait(
    host_state: &Arc<(Mutex<HostState>, Condvar)>,
    envelope: serde_json::Value,
    rpc_id: u64,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let body = format!("{}\n", envelope);
    {
        let (lock, cv) = &**host_state;
        let mut s = lock.lock().expect("host_state poisoned");
        // Wait up to 5s for a host to connect — covers the case of an
        // MCP client booting `serve` before the user has loaded the
        // extension.
        let wait_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while s.write_half.is_none() {
            let remaining = wait_deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(anyhow!(
                    "no Chrome host connected — load the extension and click connect"
                ));
            }
            let (n, _) = cv
                .wait_timeout(s, remaining)
                .expect("host_state cv poisoned");
            s = n;
        }
        let mut w = s.write_half.as_ref().unwrap().try_clone()?;
        w.write_all(body.as_bytes())?;
        w.flush().ok();
    }

    // Poll the inbox for a matching response. Cv.wait_timeout wakes us
    // each time the host pushes a line, so this is O(inbox-len-after-
    // matching-id) in the common case.
    let deadline = std::time::Instant::now() + timeout;
    let (lock, cv) = &**host_state;
    let mut s = lock.lock().expect("host_state poisoned");
    loop {
        // Scan the inbox for a response whose id matches.
        if let Some((idx, parsed)) = s.inbox.iter().enumerate().find_map(|(i, line)| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| {
                    if v.get("id").and_then(|x| x.as_u64()) == Some(rpc_id) {
                        Some((i, v))
                    } else {
                        None
                    }
                })
        }) {
            s.inbox.remove(idx);
            return Ok(parsed);
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!("RPC timeout waiting for id {rpc_id}"));
        }
        let (n, _) = cv
            .wait_timeout(s, remaining)
            .expect("host_state cv poisoned");
        s = n;
    }
}

fn tool_description(name: &str) -> &'static str {
    match name {
        "click" => "Click an element by CSS selector on the active tab.",
        "navigate" => "Navigate the active tab to a URL.",
        "ariaSnapshot" => "Capture a ref-tagged accessibility tree of the active tab.",
        "captureVisibleTab" => "Take a PNG screenshot of the active tab (returns a data URL).",
        "debuggerAttach" => {
            "Attach chrome.debugger to the active tab; required before debugger* / v8* tools."
        }
        "v8HeapSnapshot" => "Take a V8 heap snapshot via HeapProfiler.takeHeapSnapshot.",
        "v8ProfileStart" => "Start CPU profiling via Profiler.start.",
        "v8ProfileStop" => "Stop CPU profiling and return the profile.",
        _ => "Bridge method exposed by the crabcc Chrome extension.",
    }
}

fn emit_ok(stdout: &io::Stdout, id: serde_json::Value, result: serde_json::Value) {
    let resp = JsonRpcResponse::<serde_json::Value> {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    };
    write_response(stdout, &resp);
}

fn emit_error(stdout: &io::Stdout, id: serde_json::Value, code: i32, message: &str) {
    let resp = JsonRpcResponse::<serde_json::Value> {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
        }),
    };
    write_response(stdout, &resp);
}

fn write_response<T: Serialize>(stdout: &io::Stdout, resp: &JsonRpcResponse<T>) {
    let line = match serde_json::to_string(resp) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "serialise response");
            return;
        }
    };
    let mut out = stdout.lock();
    let _ = writeln!(out, "{}", line);
    let _ = out.flush();
}
