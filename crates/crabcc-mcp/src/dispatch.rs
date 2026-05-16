//! JSON-RPC method routing + tool dispatch.
//!
//! `handle_with(req, root, dev)` is the entry point used by every
//! transport. It maps the RPC `method` (initialize / tools/list /
//! tools/call) to either an inline response or to `dispatch_tool_inner`,
//! which routes to the per-tool handler.

use crate::memory;
use crate::schema::{arg_str, tools_def_for};
use crate::{
    dev_mode_from_env, error_response, hits_to_ndjson, list_indexed_files, load_or_build_graph,
    parse_mode, since_filter, want_stream, OPENAPI_YAML,
};
use anyhow::Result;
use crabcc_core::{fts::Fts, index, outline, query, store::Store};
use serde_json::{json, Value};
use std::path::Path;

pub fn handle(req: &Value, root: &Path) -> Value {
    handle_with(req, root, dev_mode_from_env())
}

/// Same as [`handle`] but takes the dev flag explicitly. Existing
/// integration tests stay on `handle()` (which reads the env var); new
/// tests exercising the default vs. dev surfaces use this.
pub fn handle_with(req: &Value, root: &Path, dev: bool) -> Value {
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
            "result": { "tools": tools_def_for(dev) }
        }),
        "tools/call" => match dispatch_tool_with(req.get("params"), root, dev) {
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

/// Dispatch the server-meta tools. Returns `Ok(Some(text))` when handled,
/// `Ok(None)` when the tool isn't a meta tool (caller continues routing).
fn dispatch_meta(tool: &str, _args: &Value) -> Result<Option<String>> {
    match tool {
        "_openapi" => Ok(Some(OPENAPI_YAML.to_string())),
        "_health" => {
            let body = json!({
                "status": "ok",
                "server": "crabcc-mcp",
                "version": env!("CARGO_PKG_VERSION"),
                "protocol_version": "2024-11-05",
                "tool_count": crate::schema::tools_def().len(),
            });
            Ok(Some(body.to_string()))
        }
        _ => Ok(None),
    }
}

fn dispatch_tool_with(params: Option<&Value>, root: &Path, dev: bool) -> Result<String> {
    let started = std::time::Instant::now();
    let p = params.ok_or_else(|| anyhow::anyhow!("missing params"))?;
    let tool = p
        .get("name")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing tool name"))?;
    let args = p.get("arguments").cloned().unwrap_or(json!({}));
    tracing::debug!(target: "crabcc_mcp", tool, "dispatch: enter");
    let result = dispatch_tool_inner(tool, args, root, dev);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => tracing::info!(target: "crabcc_mcp", tool, elapsed_ms, "dispatch: ok"),
        Err(e) => {
            tracing::warn!(target: "crabcc_mcp", tool, elapsed_ms, error = %e, "dispatch: error")
        }
    }
    result
}

fn dispatch_tool_inner(tool: &str, args: Value, root: &Path, dev: bool) -> Result<String> {
    // Meta tools are dispatched before any filesystem work: they describe
    // the server itself (OpenAPI surface, version, tool count) and must
    // succeed even on a non-repo cwd. Gated behind the dev surface — a
    // default (non-dev) call that names a meta tool returns a normal
    // "unknown tool" error, matching what `tools/list` advertised.
    if dev {
        if let Some(meta) = dispatch_meta(tool, &args)? {
            return Ok(meta);
        }
    } else if matches!(tool, "_openapi" | "_health") {
        return Err(anyhow::anyhow!(
            "tool {tool:?} is dev-only; restart the MCP server with --dev or CRABCC_MCP_DEV=1"
        ));
    }

    // `ctx` is a meta-tool that dispatches by `tool` arg name. Re-enter
    // dispatch_tool_inner with the named tool + unwrapped args so the
    // response shape matches calling that tool directly. Guard against
    // a `ctx` arg pointing at `ctx` itself — that's an infinite loop.
    if tool == "ctx" {
        let inner_tool = args
            .get("tool")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("ctx: missing `tool` arg"))?;
        if inner_tool == "ctx" {
            return Err(anyhow::anyhow!("ctx: cannot dispatch ctx -> ctx"));
        }
        let inner_args = args.get("args").cloned().unwrap_or_else(|| json!({}));
        return dispatch_tool_inner(inner_tool, inner_args, root, dev);
    }

    // Memory tools open .crabcc/memory.db directly via Palace; no symbol
    // Store needed. Route them first so we don't pay Store::open on a
    // memory-only call.
    if tool.starts_with("memory.") {
        return memory::dispatch(tool, &args, root);
    }

    let db = root.join(".crabcc").join("index.db");
    std::fs::create_dir_all(db.parent().unwrap())?;
    let store = Store::open(&db)?;

    match tool {
        "sym" => {
            let name = arg_str(&args, "name")?;
            let since_files = since_filter(&args, root)?;
            let r = match since_files.as_ref() {
                Some(set) => query::find_symbol_in_files(&store, name, set)?,
                None => query::find_symbol(&store, name)?,
            };
            memory::auto_capture(root, "sym", name, r.len(), &args);
            Ok(serde_json::to_string(&r)?)
        }
        "refs" => {
            let name = arg_str(&args, "name")?;
            let mode = parse_mode(&args);
            let since_files = since_filter(&args, root)?;
            let r = query::query_refs(&store, root, name, mode, since_files.as_ref())?;
            memory::auto_capture(root, "refs", name, r.count(), &args);
            if want_stream(&args) {
                return hits_to_ndjson(&r);
            }
            let body = serde_json::to_string(&r)?;
            Ok(crabcc_core::hash::fingerprint_envelope(
                &body,
                args.get("if_changed").and_then(|v| v.as_str()),
            ))
        }
        "callers" => {
            let name = arg_str(&args, "name")?;
            let mode = parse_mode(&args);
            let since_files = since_filter(&args, root)?;
            let r = query::query_callers(&store, root, name, mode, since_files.as_ref())?;
            memory::auto_capture(root, "callers", name, r.count(), &args);
            if want_stream(&args) {
                return hits_to_ndjson(&r);
            }
            let body = serde_json::to_string(&r)?;
            Ok(crabcc_core::hash::fingerprint_envelope(
                &body,
                args.get("if_changed").and_then(|v| v.as_str()),
            ))
        }
        "outline" => {
            let r = outline::outline(&store, arg_str(&args, "file")?)?;
            Ok(serde_json::to_string(&r)?)
        }
        "read" => {
            let path = std::path::PathBuf::from(arg_str(&args, "path")?);
            let mode_raw = args.get("mode").and_then(|v| v.as_str()).unwrap_or("auto");
            let mode = crabcc_memory::read::ReadMode::parse(mode_raw)?;
            let session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty());
            let threshold = args
                .get("threshold")
                .and_then(|v| v.as_f64())
                .unwrap_or(2.5);
            let value =
                crabcc_memory::read::compute(root, &store, path, mode, session_id, threshold)?;
            Ok(serde_json::to_string(&value)?)
        }
        "files" => {
            let under = args.get("under").and_then(|v| v.as_str());
            let lang = args.get("lang").and_then(|v| v.as_str());
            let ext = args.get("ext").and_then(|v| v.as_str());
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let r = list_indexed_files(&store, under, lang, ext, limit)?;
            Ok(serde_json::to_string(&r)?)
        }
        "index" => {
            let started = std::time::Instant::now();
            let r = index::full_index(root, &store)?;
            // Logs flag: when truthy, return an envelope with stats +
            // elapsed_ms + (empty here — in-process logs aren't piped).
            // Mirrors the shape of /api/reindex from crabcc-viz so the
            // /live dashboard and MCP clients consume identical JSON.
            if args.get("logs").and_then(|v| v.as_bool()).unwrap_or(false) {
                let env = serde_json::json!({
                    "stats": r,
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                    "logs": Vec::<String>::new(),
                });
                return Ok(env.to_string());
            }
            Ok(serde_json::to_string(&r)?)
        }
        "refresh" => {
            let want_delta = args.get("delta").and_then(|v| v.as_bool()).unwrap_or(false);
            if want_delta {
                let d = index::refresh_delta(root, &store)?;
                Ok(serde_json::to_string(&d)?)
            } else {
                let r = index::refresh(root, &store)?;
                Ok(serde_json::to_string(&r)?)
            }
        }
        "fuzzy" => {
            let q = arg_str(&args, "query")?;
            let fts = Fts::open(&root.join(".crabcc").join("tantivy"))?;
            let r = fts.fuzzy(q, 20)?;
            memory::auto_capture(root, "fuzzy", q, r.len(), &args);
            Ok(serde_json::to_string(&r)?)
        }
        "prefix" => {
            let q = arg_str(&args, "query")?;
            let fts = Fts::open(&root.join(".crabcc").join("tantivy"))?;
            let r = fts.prefix(q, 20)?;
            memory::auto_capture(root, "prefix", q, r.len(), &args);
            Ok(serde_json::to_string(&r)?)
        }
        "graph" => {
            let name = arg_str(&args, "name")?.to_string();
            let dir = args
                .get("dir")
                .and_then(|v| v.as_str())
                .unwrap_or("callers");
            let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
            let g = load_or_build_graph(&store, root)?;
            let hits = if dir == "callees" {
                g.outgoing(&name, depth)
            } else {
                g.incoming(&name, depth)
            };
            Ok(serde_json::to_string(&hits)?)
        }
        "graph_cycles" => {
            let g = load_or_build_graph(&store, root)?;
            Ok(serde_json::to_string(&g.cycles())?)
        }
        "graph_orphans" => {
            let g = load_or_build_graph(&store, root)?;
            Ok(serde_json::to_string(&g.orphans())?)
        }
        "upgrade" => {
            let repo = args
                .get("repo")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(crabcc_core::upgrade::target_repo);
            let report = crabcc_core::upgrade::build_report(&repo, Some(root));
            // The MCP path treats `apply` as opt-in just like the CLI. The
            // index store is re-opened by callers on the next tool invocation
            // — we don't try to invalidate it from here.
            if args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false) {
                let _ = crabcc_core::upgrade::cleanup_index(root);
            }
            Ok(serde_json::to_string(&report)?)
        }
        other => Err(anyhow::anyhow!("unknown tool: {other}")),
    }
}
