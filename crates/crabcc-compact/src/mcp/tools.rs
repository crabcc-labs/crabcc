use crate::{client, config, economy::Budget};
use serde_json::{json, Value};

pub struct ToolResult {
    pub content: Value,
    pub is_error: bool,
}

pub fn list_tools() -> Value {
    json!({
        "tools": [
            {
                "name": "compact.compress",
                "description": "Compress a text payload via LLMLingua-2 on the tailnet node.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"},
                        "ratio": {"type": "number", "description": "0.0-1.0, default 0.5"}
                    },
                    "required": ["text"]
                }
            },
            {
                "name": "compact.enrich",
                "description": "Enrich compressed code context with a structured attack plan.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"},
                        "query": {"type": "string"}
                    },
                    "required": ["text", "query"]
                }
            },
            {
                "name": "compact.status",
                "description": "Check the tailnet compact-server health.",
                "inputSchema": {"type": "object", "properties": {}}
            },
            {
                "name": "compact.economy",
                "description": "Return session token savings stats.",
                "inputSchema": {"type": "object", "properties": {}}
            }
        ]
    })
}

pub fn call_tool(name: &str, params: &Value) -> ToolResult {
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => return err(format!("config error: {e}")),
    };

    match name {
        "compact.compress" => {
            let text = match params.get("text").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => return err("missing 'text' parameter"),
            };
            let ratio = params.get("ratio").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
            match client::compact(&cfg.endpoint, text, ratio, cfg.timeout_ms) {
                Ok(r) => ToolResult {
                    content: json!({
                        "compressed": r.compressed,
                        "original_tokens": r.original_tokens,
                        "compressed_tokens": r.compressed_tokens,
                        "ratio": r.compressed_tokens as f32 / r.original_tokens.max(1) as f32
                    }),
                    is_error: false,
                },
                Err(e) => err(format!("compact failed: {e}")),
            }
        }
        "compact.enrich" => {
            let text = match params.get("text").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => return err("missing 'text' parameter"),
            };
            let query = match params.get("query").and_then(|v| v.as_str()) {
                Some(q) => q,
                None => return err("missing 'query' parameter"),
            };
            match client::enrich(&cfg.endpoint, text, query, cfg.timeout_ms) {
                Ok(r) => ToolResult {
                    content: json!({"plan": r.plan}),
                    is_error: false,
                },
                Err(e) => err(format!("enrich failed: {e}")),
            }
        }
        "compact.status" => match client::health(&cfg.endpoint, cfg.timeout_ms) {
            Ok(v) => ToolResult {
                content: v,
                is_error: false,
            },
            Err(e) => err(format!("unreachable: {e}")),
        },
        "compact.economy" => {
            let b = Budget::new();
            ToolResult {
                content: json!({
                    "tokens_saved": b.tokens_saved(),
                    "calls": b.calls,
                    "dedup_hits": b.dedup_hits
                }),
                is_error: false,
            }
        }
        _ => err(format!("unknown tool: {name}")),
    }
}

fn err(msg: impl Into<String>) -> ToolResult {
    ToolResult {
        content: json!({"error": msg.into()}),
        is_error: true,
    }
}
