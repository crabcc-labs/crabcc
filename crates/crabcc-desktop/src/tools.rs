//! MCP tool registry — `tools/list` + `tools/call` surface.
//!
//! The desktop publishes a small set of host-mediated tools that
//! connected MCP peers (containerised agents, the iPhone bridge,
//! Claude Code) can call without holding any host credentials.
//! This module is the registry + dispatch layer; the
//! `mcp_server::handle_connection` loop routes inbound `tools/*`
//! requests through it.
//!
//! Per `MCP-NATIVE.md` §3.1 the eventual surface includes
//! `desktop.memory.*`, `desktop.notify`, `desktop.command.run`,
//! etc. This slice ships only the wire infrastructure plus a
//! single trivial built-in (`desktop.echo`) that proves the
//! end-to-end loop without coupling to AppState. Real tools land
//! in their own slices once the snapshot pattern from
//! `crate::resources` is generalised.
//!
//! Spec target: `MCP-NATIVE.md` §3.1 (tool surface) +
//! `MCP-CONSENT.md` §5 (sensitive-tool taxonomy — applies once
//! we wire write-side tools).

use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

/// One registered tool. Cheap to clone — handler is behind Arc.
#[derive(Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON Schema for `arguments`. Returned verbatim in
    /// `tools/list` so callers (incl. LLM-driven agents) know
    /// what to pass.
    pub input_schema: Value,
    pub handler: Arc<dyn ToolHandler>,
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input_schema", &self.input_schema)
            .finish_non_exhaustive()
    }
}

/// Sync handler — runs on the per-connection thread inside the
/// MCP server. Long-running tools should fan out internally
/// (flume → background worker → reply); the trait stays sync to
/// match the rest of the desktop's blocking I/O style.
pub trait ToolHandler: Send + Sync {
    fn handle(&self, args: Value) -> Result<ToolResult, ToolError>;
}

/// MCP-shaped tool result. `is_error: true` is the convention for
/// "the tool ran but the operation failed" — distinct from
/// protocol-level errors (unknown tool, bad args) which surface
/// as JSON-RPC errors at the connection layer.
#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub content: Vec<ToolContent>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolContent {
    Text { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolErrorKind {
    /// No tool with that name in the registry. Surfaces as
    /// JSON-RPC -32602 (invalid params) at the connection layer.
    NotFound,
    /// Args failed validation. -32602.
    InvalidArgs,
    /// Internal failure mid-tool. -32603.
    Internal,
}

#[derive(Debug, Clone)]
pub struct ToolError {
    pub kind: ToolErrorKind,
    pub message: String,
}

impl ToolError {
    pub fn not_found(name: &str) -> Self {
        Self {
            kind: ToolErrorKind::NotFound,
            message: format!("tool not found: {name}"),
        }
    }
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            kind: ToolErrorKind::InvalidArgs,
            message: message.into(),
        }
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: ToolErrorKind::Internal,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for ToolError {}

/// Tiny linear-scan registry. Lookups are O(N) but N is the
/// double-digit small set of host-mediated tools; a HashMap would
/// add allocation overhead for no gain at this size.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Tool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_defaults() -> Self {
        let mut r = Self::default();
        register_builtins(&mut r);
        r
    }

    pub fn register(&mut self, tool: Tool) {
        self.tools.push(tool);
    }

    pub fn list(&self) -> &[Tool] {
        &self.tools
    }

    /// Render `tools/list` response shape per MCP spec.
    /// Returns the `tools` array (without the outer object).
    pub fn render_list(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                })
            })
            .collect()
    }

    pub fn call(&self, name: &str, args: Value) -> Result<ToolResult, ToolError> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| ToolError::not_found(name))?;
        tool.handler.handle(args)
    }
}

// ───────────────────────────────────────── built-in tools

/// Wire the always-on built-in tools into a registry. Currently
/// just `desktop.echo` — the end-to-end smoke test that proves
/// the `tools/list`+`tools/call` round trip without touching
/// AppState. Real desktop tools (`desktop.memory.*`,
/// `desktop.notify`, etc.) land in follow-up slices that adopt
/// the `crate::resources` snapshot pattern for AppState reads.
pub fn register_builtins(registry: &mut ToolRegistry) {
    registry.register(echo_tool());
}

fn echo_tool() -> Tool {
    struct EchoHandler;
    impl ToolHandler for EchoHandler {
        fn handle(&self, args: Value) -> Result<ToolResult, ToolError> {
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::invalid_args("missing string field `text`"))?;
            Ok(ToolResult {
                content: vec![ToolContent::Text {
                    text: text.to_string(),
                }],
                is_error: false,
            })
        }
    }
    Tool {
        name: "desktop.echo".into(),
        description: "Echo `text` back unchanged. Connectivity smoke test for `tools/call`.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Arbitrary string to echo back."
                }
            },
            "required": ["text"]
        }),
        handler: Arc::new(EchoHandler),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_lists_nothing() {
        let r = ToolRegistry::new();
        assert!(r.list().is_empty());
        assert!(r.render_list().is_empty());
    }

    #[test]
    fn defaults_include_echo() {
        let r = ToolRegistry::with_defaults();
        let names: Vec<_> = r.list().iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"desktop.echo"));
    }

    #[test]
    fn call_unknown_tool_returns_not_found() {
        let r = ToolRegistry::with_defaults();
        let err = r.call("does.not.exist", Value::Null).unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::NotFound);
    }

    #[test]
    fn echo_returns_text_content() {
        let r = ToolRegistry::with_defaults();
        let result = r
            .call("desktop.echo", serde_json::json!({"text": "hello"}))
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            ToolContent::Text { text } => assert_eq!(text, "hello"),
        }
    }

    #[test]
    fn echo_rejects_missing_text_field_with_invalid_args() {
        let r = ToolRegistry::with_defaults();
        let err = r.call("desktop.echo", serde_json::json!({})).unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::InvalidArgs);
    }

    #[test]
    fn echo_rejects_non_string_text() {
        let r = ToolRegistry::with_defaults();
        let err = r
            .call("desktop.echo", serde_json::json!({"text": 42}))
            .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::InvalidArgs);
    }

    #[test]
    fn render_list_includes_input_schema() {
        let r = ToolRegistry::with_defaults();
        let rendered = r.render_list();
        let echo = rendered
            .iter()
            .find(|v| v["name"] == "desktop.echo")
            .expect("echo present");
        assert_eq!(echo["inputSchema"]["type"], "object");
        assert_eq!(echo["inputSchema"]["required"][0], "text");
    }

    #[test]
    fn register_a_custom_tool_round_trips() {
        struct StubHandler;
        impl ToolHandler for StubHandler {
            fn handle(&self, _args: Value) -> Result<ToolResult, ToolError> {
                Ok(ToolResult {
                    content: vec![ToolContent::Text {
                        text: "stub".into(),
                    }],
                    is_error: false,
                })
            }
        }
        let mut r = ToolRegistry::new();
        r.register(Tool {
            name: "test.stub".into(),
            description: "for tests".into(),
            input_schema: serde_json::json!({"type": "object"}),
            handler: Arc::new(StubHandler),
        });
        let result = r.call("test.stub", Value::Null).unwrap();
        match &result.content[0] {
            ToolContent::Text { text } => assert_eq!(text, "stub"),
        }
    }
}
