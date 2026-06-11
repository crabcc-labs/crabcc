//! `crabcc gateway serve` — MCP gateway that aggregates multiple upstream MCP
//! servers behind a single stdio endpoint.
//!
//! **Config** (`~/.crabcc/gateway.toml` by default):
//! ```toml
//! [[server]]
//! name = "crabcc"
//! command = ["crabcc", "--mcp"]
//!
//! [[server]]
//! name = "fs"
//! command = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
//! ```
//!
//! **Wire format:** JSON-RPC 2.0 newline-delimited (same as `crabcc --mcp`).
//!
//! **Tool namespacing:** every tool from upstream `name` is exposed as
//! `name.tool_name`. `tools/call` routes by splitting on the first `.`.
//!
//! **Phase 1 scope:**
//! - initialize handshake (pass-through to upstreams, merge capabilities)
//! - tools/list fan-out + merge
//! - tools/call routing
//! - notifications/initialized (fire-and-forget to all upstreams)
//!
//! Deferred: tool-groups/ACLs, HTTP transport, `crabcc gateway init` scaffold.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub struct GatewayConfig {
    #[serde(default)]
    pub server: Vec<ServerEntry>,
}

#[derive(Deserialize)]
pub struct ServerEntry {
    pub name: String,
    pub command: Vec<String>,
}

impl GatewayConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let src = std::fs::read_to_string(path)
            .with_context(|| format!("read gateway config {}", path.display()))?;
        toml::from_str(&src)
            .with_context(|| format!("parse gateway config {}", path.display()))
    }

    pub fn default_path() -> PathBuf {
        let base = std::env::var("CRABCC_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".crabcc"))
                    .unwrap_or_else(|_| PathBuf::from(".crabcc"))
            });
        base.join("gateway.toml")
    }
}

// ── Upstream handle ───────────────────────────────────────────────────────────

struct Upstream {
    name: String,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    _child: Child,
}

impl Upstream {
    fn spawn(entry: &ServerEntry) -> Result<Self> {
        let (bin, args) = entry
            .command
            .split_first()
            .context("server command must not be empty")?;
        let mut child = Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawn upstream '{}'", entry.name))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let reader = BufReader::new(child.stdout.take().expect("piped stdout"));
        Ok(Upstream {
            name: entry.name.clone(),
            stdin,
            reader,
            _child: child,
        })
    }

    /// Send a JSON-RPC request and return the response. Blocks.
    fn roundtrip(&mut self, req: &Value) -> Result<Value> {
        let mut bytes = serde_json::to_vec(req)?;
        bytes.push(b'\n');
        self.stdin
            .write_all(&bytes)
            .with_context(|| format!("write to upstream '{}'", self.name))?;
        self.stdin.flush()?;
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .with_context(|| format!("read from upstream '{}'", self.name))?;
        serde_json::from_str(line.trim())
            .with_context(|| format!("parse response from upstream '{}'", self.name))
    }

    /// Fire-and-forget: send without reading the response (used for notifications).
    fn notify(&mut self, msg: &Value) {
        if let Ok(mut bytes) = serde_json::to_vec(msg) {
            bytes.push(b'\n');
            let _ = self.stdin.write_all(&bytes);
            let _ = self.stdin.flush();
        }
    }
}

// ── Gateway router ────────────────────────────────────────────────────────────

struct Gateway {
    upstreams: Vec<Upstream>,
}

impl Gateway {
    fn new(upstreams: Vec<Upstream>) -> Self {
        Self { upstreams }
    }

    /// Send `initialize` to all upstreams; merge capabilities; return a merged
    /// server-info response to the client.
    fn initialize(&mut self, req: &Value) -> Value {
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let mut merged_tools = json!({"listChanged": false});
        let mut server_names: Vec<String> = Vec::new();

        for up in &mut self.upstreams {
            match up.roundtrip(req) {
                Ok(resp) => {
                    server_names.push(up.name.clone());
                    // Merge tool capabilities
                    if let Some(tools) = resp
                        .get("result")
                        .and_then(|r| r.get("capabilities"))
                        .and_then(|c| c.get("tools"))
                    {
                        if tools.get("listChanged") == Some(&Value::Bool(true)) {
                            merged_tools["listChanged"] = Value::Bool(true);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(upstream = %up.name, err = %e, "initialize failed");
                }
            }
        }

        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": merged_tools
                },
                "serverInfo": {
                    "name": "crabcc-gateway",
                    "version": env!("CARGO_PKG_VERSION"),
                    "upstreams": server_names
                }
            }
        })
    }

    /// Fan-out `tools/list` to all upstreams; namespace each tool as `<upstream>.<name>`.
    fn tools_list(&mut self, req: &Value) -> Value {
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let mut all_tools: Vec<Value> = Vec::new();

        for up in &mut self.upstreams {
            let list_req = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {}
            });
            match up.roundtrip(&list_req) {
                Ok(resp) => {
                    if let Some(tools) = resp
                        .get("result")
                        .and_then(|r| r.get("tools"))
                        .and_then(Value::as_array)
                    {
                        for tool in tools {
                            let mut namespaced = tool.clone();
                            if let Some(name) = tool.get("name").and_then(Value::as_str) {
                                namespaced["name"] =
                                    Value::String(format!("{}.{name}", up.name));
                            }
                            // Prefix description so callers know the routing
                            if let Some(desc) = tool.get("description").and_then(Value::as_str) {
                                namespaced["description"] =
                                    Value::String(format!("[{}] {desc}", up.name));
                            }
                            all_tools.push(namespaced);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(upstream = %up.name, err = %e, "tools/list failed");
                }
            }
        }

        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": all_tools }
        })
    }

    /// Route `tools/call` to the upstream whose name matches the prefix of the
    /// tool name (`<upstream>.<tool>`). Strips the prefix before forwarding.
    fn tools_call(&mut self, req: &Value) -> Value {
        let id = req.get("id").cloned().unwrap_or(Value::Null);

        let tool_name = req
            .get("params")
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");

        // Split "upstream.tool_name" on first '.'
        let (prefix, bare) = match tool_name.split_once('.') {
            Some(pair) => pair,
            None => {
                return json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32602,
                        "message": format!(
                            "tool name '{tool_name}' must be namespaced as '<server>.<tool>'"
                        )
                    }
                });
            }
        };

        let up = match self.upstreams.iter_mut().find(|u| u.name == prefix) {
            Some(u) => u,
            None => {
                return json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("no upstream named '{prefix}'")
                    }
                });
            }
        };

        // Rewrite the request: strip namespace prefix, preserve all other params
        let mut forwarded = req.clone();
        if let Some(params) = forwarded.get_mut("params") {
            params["name"] = Value::String(bare.to_string());
        }

        match up.roundtrip(&forwarded) {
            Ok(resp) => resp,
            Err(e) => {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32603, "message": format!("upstream error: {e}") }
                })
            }
        }
    }

    /// Dispatch a single JSON-RPC request and return the response to write to
    /// stdout. Returns `None` for notifications (no response needed).
    fn dispatch(&mut self, req: Value) -> Option<Value> {
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let is_notification = req.get("id").is_none();

        match method {
            "initialize" => Some(self.initialize(&req)),
            "notifications/initialized" => {
                for up in &mut self.upstreams {
                    up.notify(&req);
                }
                None
            }
            "tools/list" => Some(self.tools_list(&req)),
            "tools/call" => Some(self.tools_call(&req)),
            _ if is_notification => {
                // Forward unknown notifications to all upstreams
                for up in &mut self.upstreams {
                    up.notify(&req);
                }
                None
            }
            _ => {
                let id = req.get("id").cloned().unwrap_or(Value::Null);
                Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("method not found: {method}") }
                }))
            }
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn serve(config_path: &Path) -> Result<()> {
    let cfg = if config_path.exists() {
        GatewayConfig::load(config_path)?
    } else {
        bail!(
            "gateway config not found: {}\n\
             Create it with:\n\
             \n\
             [[server]]\n\
             name = \"crabcc\"\n\
             command = [\"crabcc\", \"--mcp\"]\n",
            config_path.display()
        );
    };

    if cfg.server.is_empty() {
        bail!("no [[server]] entries in {}", config_path.display());
    }

    tracing::info!(
        config = %config_path.display(),
        count = cfg.server.len(),
        "gateway starting"
    );

    let mut upstreams: Vec<Upstream> = Vec::with_capacity(cfg.server.len());
    for entry in &cfg.server {
        let up = Upstream::spawn(entry)?;
        tracing::info!(name = %entry.name, cmd = ?entry.command, "upstream spawned");
        upstreams.push(up);
    }

    let mut gw = Gateway::new(upstreams);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    let mut buf = Vec::with_capacity(4096);

    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break, // EOF — client disconnected
            Ok(_) => {}
            Err(e) => {
                tracing::error!(err = %e, "stdin read error");
                break;
            }
        }

        let req: Value = match serde_json::from_slice(&buf) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(err = %e, "malformed JSON on stdin");
                let err_resp = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") }
                });
                let _ = serde_json::to_writer(&mut writer, &err_resp);
                let _ = writer.write_all(b"\n");
                let _ = writer.flush();
                continue;
            }
        };

        if let Some(resp) = gw.dispatch(req) {
            if let Err(e) = serde_json::to_writer(&mut writer, &resp) {
                tracing::error!(err = %e, "stdout write error");
                break;
            }
            if let Err(e) = writer.write_all(b"\n") {
                tracing::error!(err = %e, "stdout newline error");
                break;
            }
            if let Err(e) = writer.flush() {
                tracing::error!(err = %e, "stdout flush error");
                break;
            }
        }
    }

    Ok(())
}
