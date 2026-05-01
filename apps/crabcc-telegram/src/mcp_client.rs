//! MCP HTTP client for the telegram bot (#204 phase 2).
//!
//! Replaces the bot's `TokioCommand::new("crabcc")` subprocess shape
//! with a `POST /mcp` call to a host-side `crabcc-mcp` HTTP server.
//! Lets the bot run inside a distroless container without needing a
//! host-mounted `crabcc` binary.
//!
//! Config: `MCP_ENDPOINT` env var (default: `http://host.docker.internal:8091/mcp`).
//! Optional `MCP_AUTH_TOKEN` adds a `Authorization: Bearer …` header
//! on every request — must match the server's `MCP_AUTH_TOKEN`.

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

/// Client config — loaded once at bot startup, passed through state.
#[derive(Clone, Debug)]
pub struct McpConfig {
    pub endpoint: String,
    pub token: Option<String>,
    pub timeout: Duration,
}

impl McpConfig {
    /// Read from env. Default endpoint mirrors the docker bot's
    /// `host.docker.internal` hairpin to the host's loopback. Native
    /// (non-container) bot runs work too — `host.docker.internal`
    /// resolves to localhost on Mac with Docker Desktop / OrbStack.
    pub fn from_env() -> Result<Self> {
        let endpoint = std::env::var("MCP_ENDPOINT")
            .unwrap_or_else(|_| "http://host.docker.internal:8091/mcp".to_string());
        let token = std::env::var("MCP_AUTH_TOKEN")
            .ok()
            .filter(|t| !t.is_empty());
        Ok(Self {
            endpoint,
            token,
            timeout: Duration::from_secs(125),
        })
    }
}

/// Decide whether a `crabcc <args…>` invocation should be detached.
///
/// Long-running tools (currently just `agent --run`) need
/// `detached: true` so the MCP server returns immediately rather
/// than blocking until the agent finishes. Sync tools (status,
/// search, doctor, index, kill) get the standard request/response
/// flow.
pub fn is_long_running(args: &[&str]) -> bool {
    args.first().copied() == Some("agent") && args.iter().any(|a| *a == "--run")
}

/// Run an allowlisted `crabcc` subcommand via the host-side
/// `cli.run` MCP tool. Mirrors the old `crabcc(args)` helper's
/// `Result<String>` signature so existing call sites are byte-
/// for-byte compatible.
pub async fn run(http: &Client, cfg: &McpConfig, args: &[&str]) -> Result<String> {
    let detached = is_long_running(args);
    let cmd: Vec<String> = args.iter().map(|s| s.to_string()).collect();

    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "cli.run",
            "arguments": {
                "cmd": cmd,
                "detached": detached,
                "timeout_secs": 120,
            }
        }
    });

    let mut req = http.post(&cfg.endpoint).timeout(cfg.timeout).json(&body);
    if let Some(t) = &cfg.token {
        req = req.bearer_auth(t);
    }

    let resp = req.send().await.context("POST /mcp")?;
    let status = resp.status();
    let raw_body = resp.text().await.context("read mcp body")?;
    if !status.is_success() {
        return Err(anyhow!("mcp http {status}: {raw_body}"));
    }

    // JSON-RPC envelope:
    //   {"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"…"}]}}
    let envelope: Value = serde_json::from_str(&raw_body)
        .with_context(|| format!("mcp body not JSON: {raw_body}"))?;
    if let Some(err) = envelope.get("error") {
        return Err(anyhow!("mcp error: {err}"));
    }
    let text = envelope
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("mcp envelope missing /result/content/0/text: {raw_body}"))?;

    // The text is the JSON `dispatch_cli_run` produced. Parse it
    // back so we can shape a friendly String for the Telegram reply.
    let payload: Value =
        serde_json::from_str(text).with_context(|| format!("cli.run payload not JSON: {text}"))?;

    if payload
        .get("spawned")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let pid = payload.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
        return Ok(format!("🦀 spawned (pid {pid})"));
    }

    if payload
        .get("timed_out")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let secs = payload
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        return Ok(format!(
            "⌛ subprocess timed out after {secs}s — try a narrower request or check `crabcc doctor`"
        ));
    }

    let stdout = payload.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
    let stderr = payload.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
    let exit_code = payload
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);

    if exit_code == 0 {
        Ok(if stdout.is_empty() {
            "Done.".into()
        } else {
            stdout.to_string()
        })
    } else {
        Ok(format!(
            "error (exit {exit_code}):\n{}",
            if stderr.is_empty() { stdout } else { stderr }
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_only_for_agent_run() {
        assert!(is_long_running(&["agent", "--run", "task"]));
        assert!(is_long_running(&[
            "agent",
            "--backend",
            "ollama",
            "--run",
            "task"
        ]));
        assert!(!is_long_running(&["agent-ls", "--limit", "5"]));
        assert!(!is_long_running(&["agent-kill", "abc"]));
        assert!(!is_long_running(&["doctor"]));
        assert!(!is_long_running(&["index"]));
        assert!(!is_long_running(&["memory", "search", "q"]));
    }

    #[test]
    fn config_defaults_to_host_docker_internal() {
        // Note: env var pollution if other tests set MCP_ENDPOINT.
        // Acceptable since this test runs in the bot's own test
        // process and we don't set MCP_ENDPOINT elsewhere.
        std::env::remove_var("MCP_ENDPOINT");
        std::env::remove_var("MCP_AUTH_TOKEN");
        let c = McpConfig::from_env().unwrap();
        assert_eq!(c.endpoint, "http://host.docker.internal:8091/mcp");
        assert!(c.token.is_none());
    }
}
