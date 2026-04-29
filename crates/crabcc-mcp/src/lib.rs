// crabcc MCP server (stdio).
//
// Newline-delimited JSON-RPC 2.0, per the MCP stdio transport spec.
// Implements: initialize, tools/list, tools/call.
//
// Tools exposed:
//   sym, refs, callers, outline, index, refresh
//
// All tool results are returned as a single text content block whose body
// is the same compact JSON the CLI prints to stdout. That way the agent
// gets the same payload whether it talks to crabcc via subprocess or MCP.

use anyhow::Result;
use crabcc_core::{fts::Fts, index, outline, query, store::Store};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

pub fn serve_stdio(root: &Path) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let resp = error_response(None, -32700, &format!("parse error: {e}"));
                writeln!(writer, "{resp}")?;
                writer.flush()?;
                continue;
            }
        };
        let resp = handle(&req, root);
        writeln!(writer, "{resp}")?;
        writer.flush()?;
    }
    Ok(())
}

pub fn handle(req: &Value, root: &Path) -> Value {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

    match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "crabcc-mcp",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }
        }),
        "notifications/initialized" => Value::Null,
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": tools_def() }
        }),
        "tools/call" => match dispatch_tool(req.get("params"), root) {
            Ok(content) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": content }]
                }
            }),
            Err(e) => error_response(id, -32603, &format!("tool error: {e}")),
        },
        _ => error_response(id, -32601, &format!("method not found: {method}")),
    }
}

pub fn tools_def() -> Vec<Value> {
    vec![
        tool_schema(
            "sym",
            "Find a symbol by exact name. Returns JSON array of \
             {name, kind, signature, parent, file, line_start, line_end, visibility}.",
            json!({"name": str_field("symbol name to look up")}),
            &["name"],
        ),
        tool_schema(
            "refs",
            "Find every identifier reference to `name` across the indexed repo. \
             Coarse — matches text equality on identifier nodes.",
            json!({"name": str_field("symbol name")}),
            &["name"],
        ),
        tool_schema(
            "callers",
            "Find call sites of `name` — both bare (`foo()`) and method-receiver \
             (`obj.foo()`) shapes. Returns file/line/snippet hits.",
            json!({"name": str_field("function or method name")}),
            &["name"],
        ),
        tool_schema(
            "outline",
            "All symbols in `file` ordered by line. Use parent field to reconstruct hierarchy.",
            json!({"file": str_field("repo-relative file path")}),
            &["file"],
        ),
        tool_schema(
            "index",
            "Build a fresh full index (wipes existing).",
            json!({}),
            &[],
        ),
        tool_schema(
            "refresh",
            "Incremental refresh: mtime + sha256 diff vs stored.",
            json!({}),
            &[],
        ),
        tool_schema(
            "fuzzy",
            "Fuzzy symbol-name search (Levenshtein distance 2). Use when the \
             user might mistype or remember the name approximately.",
            json!({"query": str_field("partial or misspelled symbol name")}),
            &["query"],
        ),
        tool_schema(
            "prefix",
            "Prefix symbol-name search (case-insensitive starts-with).",
            json!({"query": str_field("symbol-name prefix")}),
            &["query"],
        ),
    ]
}

fn str_field(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}

fn tool_schema(name: &str, desc: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": desc,
        "inputSchema": {
            "type": "object",
            "properties": props,
            "required": required,
        }
    })
}

fn dispatch_tool(params: Option<&Value>, root: &Path) -> Result<String> {
    let p = params.ok_or_else(|| anyhow::anyhow!("missing params"))?;
    let tool = p
        .get("name")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing tool name"))?;
    let args = p.get("arguments").cloned().unwrap_or(json!({}));

    let db = root.join(".crabcc").join("index.db");
    std::fs::create_dir_all(db.parent().unwrap())?;
    let store = Store::open(&db)?;

    fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
        args.get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing arg: {key}"))
    }

    match tool {
        "sym" => {
            let r = query::find_symbol(&store, arg_str(&args, "name")?)?;
            Ok(serde_json::to_string(&r)?)
        }
        "refs" => {
            let r = query::find_refs(&store, root, arg_str(&args, "name")?)?;
            Ok(serde_json::to_string(&r)?)
        }
        "callers" => {
            let r = query::find_callers(&store, root, arg_str(&args, "name")?)?;
            Ok(serde_json::to_string(&r)?)
        }
        "outline" => {
            let r = outline::outline(&store, arg_str(&args, "file")?)?;
            Ok(serde_json::to_string(&r)?)
        }
        "index" => {
            let r = index::full_index(root, &store)?;
            Ok(serde_json::to_string(&r)?)
        }
        "refresh" => {
            let r = index::refresh(root, &store)?;
            Ok(serde_json::to_string(&r)?)
        }
        "fuzzy" => {
            let fts = Fts::open(&root.join(".crabcc").join("tantivy"))?;
            let r = fts.fuzzy(arg_str(&args, "query")?, 20)?;
            Ok(serde_json::to_string(&r)?)
        }
        "prefix" => {
            let fts = Fts::open(&root.join(".crabcc").join("tantivy"))?;
            let r = fts.prefix(arg_str(&args, "query")?, 20)?;
            Ok(serde_json::to_string(&r)?)
        }
        other => Err(anyhow::anyhow!("unknown tool: {other}")),
    }
}

fn error_response(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("hi.ts"),
            "export function hello(name: string){return name;}\nhello('world');\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join(".crabcc")).unwrap();
        let store = Store::open(&dir.path().join(".crabcc").join("index.db")).unwrap();
        crabcc_core::index::full_index(dir.path(), &store).unwrap();
        dir
    }

    #[test]
    fn handle_initialize() {
        let dir = tempfile::tempdir().unwrap();
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        let resp = handle(&req, dir.path());
        assert_eq!(resp["id"], 1);
        assert!(resp["result"]["serverInfo"]["name"].as_str().unwrap().contains("crabcc"));
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn handle_tools_list_has_all_tools() {
        let dir = tempfile::tempdir().unwrap();
        let req = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let resp = handle(&req, dir.path());
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in ["sym", "refs", "callers", "outline", "index", "refresh", "fuzzy", "prefix"] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn handle_unknown_method_errors() {
        let dir = tempfile::tempdir().unwrap();
        let req = json!({"jsonrpc": "2.0", "id": 3, "method": "frobnicate"});
        let resp = handle(&req, dir.path());
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn handle_tools_call_sym_returns_json_content() {
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "sym", "arguments": { "name": "hello" } }
        });
        let resp = handle(&req, dir.path());
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "hello");
    }

    #[test]
    fn handle_tools_call_outline() {
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": { "name": "outline", "arguments": { "file": "hi.ts" } }
        });
        let resp = handle(&req, dir.path());
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert!(parsed.as_array().unwrap().len() >= 1);
    }

    #[test]
    fn handle_tools_call_missing_arg_returns_error() {
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": { "name": "sym", "arguments": {} }
        });
        let resp = handle(&req, dir.path());
        assert!(resp["error"].is_object(),
                "expected error response, got: {resp}");
    }

    #[test]
    fn handle_tools_call_unknown_tool_errors() {
        let dir = tempfile::tempdir().unwrap();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": "no_such_tool", "arguments": {} }
        });
        let resp = handle(&req, dir.path());
        assert!(resp["error"].is_object());
    }
}
