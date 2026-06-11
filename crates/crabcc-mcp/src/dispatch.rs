//! JSON-RPC method routing + tool dispatch.
//!
//! `handle_with(req, root, dev)` is the entry point used by every
//! transport. It maps the RPC `method` (initialize / tools/list /
//! tools/call) to either an inline response or to `dispatch_tool_inner`,
//! which routes to the per-tool handler.

use crate::mastodon;
use crate::memory;
use crate::ntfy;
use crate::schema::{arg_str, tools_def_for};
use crate::{
    dev_mode_from_env, error_response, hits_to_ndjson, list_indexed_files, load_or_build_graph,
    parse_mode, since_filter, want_stream, OPENAPI_YAML,
};
use anyhow::Result;
use crabcc_core::{fts::Fts, index, outline, query, store::Store};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::LazyLock;

static EMPTY_ARGS: LazyLock<Value> = LazyLock::new(|| json!({}));

pub fn handle(req: &Value, root: &Path) -> Value {
    handle_with(req, root, dev_mode_from_env())
}

/// Same as [`handle`] but takes the dev flag explicitly. Existing
/// integration tests stay on `handle()` (which reads the env var); new
/// tests exercising the default vs. dev surfaces use this.
pub fn handle_with(req: &Value, root: &Path, dev: bool) -> Value {
    // One-shot callers (HTTP transport, tests) open a fresh Store per call.
    handle_with_session(req, root, dev, &mut None)
}

/// Same as [`handle_with`] but reuses a caller-owned [`Store`] across calls.
/// `serve_io` (the long-lived stdio session) holds one `Option<Store>` for the
/// whole session so each `tools/call` skips the per-call `Store::open` (SQLite
/// open + sqlite-vec load + pragmas), which dominated per-call latency.
pub fn handle_with_session(
    req: &Value,
    root: &Path,
    dev: bool,
    store: &mut Option<Store>,
) -> Value {
    let id = req.get("id").cloned();
    let method = req
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or_default();

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
        "tools/call" => match dispatch_tool_with(req.get("params"), root, dev, store) {
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

fn dispatch_tool_with(
    params: Option<&Value>,
    root: &Path,
    dev: bool,
    store: &mut Option<Store>,
) -> Result<String> {
    let started = std::time::Instant::now();
    let p = params.ok_or_else(|| anyhow::anyhow!("missing params"))?;
    let tool = p
        .get("name")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing tool name"))?;
    let args = p.get("arguments").unwrap_or(&*EMPTY_ARGS);
    tracing::debug!(target: "crabcc_mcp", tool, "dispatch: enter");
    let result = dispatch_tool_inner(tool, args, root, dev, store);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => tracing::info!(target: "crabcc_mcp", tool, elapsed_ms, "dispatch: ok"),
        Err(e) => {
            tracing::warn!(target: "crabcc_mcp", tool, elapsed_ms, error = %e, "dispatch: error")
        }
    }
    result
}

fn dispatch_tool_inner(
    tool: &str,
    args: &Value,
    root: &Path,
    dev: bool,
    cache: &mut Option<Store>,
) -> Result<String> {
    // Meta tools are dispatched before any filesystem work: they describe
    // the server itself (OpenAPI surface, version, tool count) and must
    // succeed even on a non-repo cwd. Gated behind the dev surface — a
    // default (non-dev) call that names a meta tool returns a normal
    // "unknown tool" error, matching what `tools/list` advertised.
    if dev {
        if let Some(meta) = dispatch_meta(tool, args)? {
            return Ok(meta);
        }
    } else if matches!(tool, "_openapi" | "_health") {
        anyhow::bail!(
            "tool {tool:?} is dev-only; restart the MCP server with --dev or CRABCC_MCP_DEV=1"
        );
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
            anyhow::bail!("ctx: cannot dispatch ctx -> ctx");
        }
        let inner_args = args.get("args").unwrap_or(&*EMPTY_ARGS);
        return dispatch_tool_inner(inner_tool, inner_args, root, dev, cache);
    }

    // Mastodon tools talk to a remote instance over HTTP; no local Store
    // needed. Route them before the Store open so we don't pay SQLite
    // startup on a social-only call.
    if tool.starts_with("mastodon.") {
        return mastodon::dispatch(tool, args);
    }

    // Memory tools open .crabcc/memory.db directly via Palace; no symbol
    // Store needed. Route them first so we don't pay Store::open on a
    // memory-only call.
    if tool.starts_with("memory.") {
        return memory::dispatch(tool, args, root);
    }

    if cache.is_none() {
        let db = root.join(".crabcc").join("index.db");
        let parent = db
            .parent()
            .ok_or_else(|| anyhow::anyhow!("invalid index path: {}", db.display()))?;
        std::fs::create_dir_all(parent)?;
        *cache = Some(Store::open(&db)?);
    }
    let store = cache.as_ref().expect("cache must be Some after open");

    let result = match tool {
        "sym" => {
            let name = arg_str(args, "name")?;
            let since_files = since_filter(args, root)?;
            let r = match since_files.as_ref() {
                Some(set) => query::find_symbol_in_files(store, name, set)?,
                None => query::find_symbol(store, name)?,
            };
            memory::auto_capture(root, "sym", name, r.len(), args);
            Ok(serde_json::to_string(&r)?)
        }
        "refs" => {
            let name = arg_str(args, "name")?;
            let mode = parse_mode(args);
            let since_files = since_filter(args, root)?;
            let r = query::query_refs(store, root, name, mode, since_files.as_ref())?;
            memory::auto_capture(root, "refs", name, r.count(), args);
            if want_stream(args) {
                return hits_to_ndjson(&r);
            }
            let body = serde_json::to_string(&r)?;
            Ok(crabcc_core::hash::fingerprint_envelope(
                &body,
                args.get("if_changed").and_then(|v| v.as_str()),
            ))
        }
        "callers" => {
            let name = arg_str(args, "name")?;
            let mode = parse_mode(args);
            let since_files = since_filter(args, root)?;
            let r = query::query_callers(store, root, name, mode, since_files.as_ref())?;
            memory::auto_capture(root, "callers", name, r.count(), args);
            if want_stream(args) {
                return hits_to_ndjson(&r);
            }
            let body = serde_json::to_string(&r)?;
            Ok(crabcc_core::hash::fingerprint_envelope(
                &body,
                args.get("if_changed").and_then(|v| v.as_str()),
            ))
        }
        "outline" => {
            let r = outline::outline(store, arg_str(args, "file")?)?;
            Ok(serde_json::to_string(&r)?)
        }
        "test_context" => {
            let name = arg_str(args, "name")?;
            let file_arg = args.get("file").and_then(|v| v.as_str());
            let max_callers = args
                .get("max_callers")
                .and_then(|v| v.as_u64())
                .unwrap_or(50) as usize;
            let max_refs = args.get("max_refs").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            let blast_depth = args
                .get("blast_depth")
                .and_then(|v| v.as_u64())
                .unwrap_or(2) as usize;

            // Resolve the symbol — prefer file-scoped lookup when provided.
            let symbols = query::find_symbol(store, name)?;
            let symbol = match file_arg {
                Some(f) => symbols.into_iter().find(|s| s.file == f),
                None => symbols.into_iter().next(),
            };
            let symbol =
                symbol.ok_or_else(|| anyhow::anyhow!("test_context: symbol {name:?} not found"))?;

            // Outline of the symbol's file
            let outline_items = outline::outline(store, &symbol.file)?;

            // All callers (capped)
            let callers_output =
                query::query_callers(store, root, name, query::Mode::Hits { limit: None }, None)?;
            let callers: Vec<_> = match callers_output {
                query::Output::Hits(h) => h.into_iter().take(max_callers).collect(),
                _ => Vec::new(),
            };

            // All refs (capped)
            let refs_output =
                query::query_refs(store, root, name, query::Mode::Hits { limit: None }, None)?;
            let refs: Vec<_> = match refs_output {
                query::Output::Hits(h) => h.into_iter().take(max_refs).collect(),
                _ => Vec::new(),
            };

            // Blast radius — transitive callees via the edge graph.
            // Use blast_radius if available; fall back to empty array on
            // any error so the tool stays useful when the call-graph
            // sidecar hasn't been built yet.
            let blast = match resolve_symbol_id(store, &symbol.name) {
                Ok(id) => match query::blast_radius::blast_radius(store, id, blast_depth, &[]) {
                    Ok(v) => serde_json::to_value(v).unwrap_or(json!([])),
                    Err(_) => json!([]),
                },
                Err(_) => json!([]),
            };

            let envelope = json!({
                "symbol":        symbol,
                "outline":       outline_items,
                "callers":       callers,
                "refs":          refs,
                "blast_radius":  blast,
            });
            memory::auto_capture(root, "test_context", name, 1, args);
            Ok(serde_json::to_string(&envelope)?)
        }
        "affected" => {
            use crabcc_core::affected::{affected, ChangeInput, DEFAULT_DEPTH};
            let depth = args
                .get("depth")
                .and_then(|v| v.as_u64())
                .map(|d| d as usize)
                .unwrap_or(DEFAULT_DEPTH);
            let symbols: Vec<String> = args
                .get("symbols")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let input = if !symbols.is_empty() {
                ChangeInput::Symbols(symbols)
            } else if let Some(rev) = args.get("since").and_then(|v| v.as_str()) {
                ChangeInput::Since(rev.to_string())
            } else {
                ChangeInput::WorkingTree
            };
            let result = affected(store, root, input, depth)?;
            memory::auto_capture(root, "affected", "change", result.tests.len(), args);
            Ok(serde_json::to_string(&result)?)
        }
        "write_file" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("write_file: missing arg `path`"))?;
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("write_file: missing arg `content`"))?;
            if std::path::Path::new(path).is_absolute() {
                anyhow::bail!("write_file: path must be repo-relative");
            }
            if path.split('/').any(|c| c == "..") {
                anyhow::bail!("write_file: `..` in path is rejected");
            }
            let abs = root.join(path);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let before = store.symbols_in_file(path).unwrap_or_default();
            std::fs::write(&abs, content.as_bytes())?;
            let (after, parse_err) = crabcc_core::validate::reindex_file(store, path, content)?;
            let diff = crabcc_core::validate::diff_symbols(path, &before, &after);
            let removed = diff.removed_names();
            let broken: std::collections::BTreeSet<String> = removed
                .iter()
                .filter_map(|name| query::find_callers(store, root, name).ok())
                .flatten()
                .map(|h| h.file)
                .collect();
            memory::auto_capture(root, "write_file", path, 1, args);
            ntfy::on_write(path, content.len());
            let env = serde_json::json!({
                "wrote": { "path": path, "bytes": content.len() },
                "validation": {
                    "parse_ok": parse_err.is_none(),
                    "parse_error": parse_err,
                    "symbol_diff": diff,
                    "broken_caller_files": broken.into_iter().collect::<Vec<String>>(),
                },
            });
            Ok(serde_json::to_string(&env)?)
        }
        "read" => {
            let path = std::path::PathBuf::from(arg_str(args, "path")?);
            let mode_raw = args.get("mode").and_then(|v| v.as_str()).unwrap_or("auto");
            let mode = crabcc_memory::read::ReadMode::parse(mode_raw)?;
            let session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty());
            let threshold = args
                .get("threshold")
                .and_then(|v| v.as_f64())
                .unwrap_or(2.5);
            let value =
                crabcc_memory::read::compute(root, store, path, mode, session_id, threshold)?;
            Ok(serde_json::to_string(&value)?)
        }
        "files" => {
            let under = args.get("under").and_then(|v| v.as_str());
            let lang = args.get("lang").and_then(|v| v.as_str());
            let ext = args.get("ext").and_then(|v| v.as_str());
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or_default() as usize;
            let r = list_indexed_files(store, under, lang, ext, limit)?;
            Ok(serde_json::to_string(&r)?)
        }
        "index" => {
            let started = std::time::Instant::now();
            let r = index::full_index(root, store)?;
            let elapsed_ms = started.elapsed().as_millis() as u64;
            ntfy::on_index(r.symbols, elapsed_ms);
            // Logs flag: when truthy, return an envelope with stats +
            // elapsed_ms + (empty here — in-process logs aren't piped).
            // Mirrors the shape of /api/reindex from crabcc-viz so the
            // /live dashboard and MCP clients consume identical JSON.
            if args
                .get("logs")
                .and_then(|v| v.as_bool())
                .unwrap_or_default()
            {
                let env = serde_json::json!({
                    "stats": r,
                    "elapsed_ms": elapsed_ms,
                    "logs": Vec::<String>::new(),
                });
                return Ok(env.to_string());
            }
            Ok(serde_json::to_string(&r)?)
        }
        "refresh" => {
            let want_delta = args
                .get("delta")
                .and_then(|v| v.as_bool())
                .unwrap_or_default();
            let started = std::time::Instant::now();
            if want_delta {
                let d = index::refresh_delta(root, store)?;
                let elapsed_ms = started.elapsed().as_millis() as u64;
                let updated = d.added.len() + d.modified.len();
                ntfy::on_refresh(updated, elapsed_ms);
                Ok(serde_json::to_string(&d)?)
            } else {
                let r = index::refresh(root, store)?;
                let elapsed_ms = started.elapsed().as_millis() as u64;
                let updated = r.new + r.reindexed;
                ntfy::on_refresh(updated, elapsed_ms);
                Ok(serde_json::to_string(&r)?)
            }
        }
        "fuzzy" => {
            let q = arg_str(args, "query")?;
            let fts = Fts::from_store(store)?;
            let r = fts.fuzzy(q, 20)?;
            memory::auto_capture(root, "fuzzy", q, r.len(), args);
            Ok(serde_json::to_string(&r)?)
        }
        "prefix" => {
            let q = arg_str(args, "query")?;
            let fts = Fts::from_store(store)?;
            let r = fts.prefix(q, 20)?;
            memory::auto_capture(root, "prefix", q, r.len(), args);
            Ok(serde_json::to_string(&r)?)
        }
        "graph" => {
            let name = arg_str(args, "name")?.to_string();
            let dir = args
                .get("dir")
                .and_then(|v| v.as_str())
                .unwrap_or("callers");
            let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
            let symbol_id = resolve_symbol_id(store, &name)?;
            let g = load_or_build_graph(store, root)?;
            let hits = if dir == "callees" {
                g.outgoing(symbol_id, depth)
            } else {
                g.incoming(symbol_id, depth)
            };
            Ok(serde_json::to_string(&hits)?)
        }
        "graph_cycles" => {
            let g = load_or_build_graph(store, root)?;
            Ok(serde_json::to_string(&g.cycles())?)
        }
        "graph_orphans" => {
            let g = load_or_build_graph(store, root)?;
            Ok(serde_json::to_string(&g.orphans())?)
        }
        "graph.blast_radius" => {
            let symbol = arg_str(args, "symbol")?.to_string();
            let depth = args
                .get("depth")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(5);
            let kind = args.get("kind").and_then(|v| v.as_str()).map(String::from);
            let symbol_id = resolve_symbol_id(store, &symbol)?;
            let kinds: &[&str] = match kind.as_deref() {
                Some(k) => &[k][..],
                None => &[][..],
            };
            let hits =
                crabcc_core::query::blast_radius::blast_radius(store, symbol_id, depth, kinds)?;
            Ok(serde_json::to_string(&hits)?)
        }
        "graph.why" => {
            let src = arg_str(args, "src")?.to_string();
            let dst = arg_str(args, "dst")?.to_string();
            let max_depth = args
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(8);
            let src_id = resolve_symbol_id(store, &src)?;
            let dst_id = resolve_symbol_id(store, &dst)?;
            let path = crabcc_core::query::why::why(store, src_id, dst_id, max_depth)?;
            Ok(serde_json::to_string(&path)?)
        }
        "graph.hot_symbols" => {
            let top = args
                .get("top")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(10);
            let kind = args.get("kind").and_then(|v| v.as_str()).map(String::from);
            let kinds: &[&str] = match kind.as_deref() {
                Some(k) => &[k][..],
                None => &[][..],
            };
            let hits = crabcc_core::query::hot_symbols::hot_symbols(store, top, kinds)?;
            Ok(serde_json::to_string(&hits)?)
        }
        "graph.importers" => {
            let path = arg_str(args, "path")?.to_string();
            let depth = args
                .get("depth")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(3);
            let hits = crabcc_core::query::importers::importers(store, &path, depth)?;
            Ok(serde_json::to_string(&hits)?)
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
            if args
                .get("apply")
                .and_then(|v| v.as_bool())
                .unwrap_or_default()
            {
                let _ = crabcc_core::upgrade::cleanup_index(root);
            }
            Ok(serde_json::to_string(&report)?)
        }
        other => Err(anyhow::anyhow!("unknown tool: {other}")),
    };
    // Index-lifecycle tools rebuild or remove the index artifacts; drop the
    // cached Store so the next call re-opens against the new state (matches the
    // prior open-per-call semantics for these tools).
    if matches!(tool, "index" | "refresh" | "upgrade") {
        *cache = None;
    }
    result
}

/// Resolve a user-typed symbol name to a `symbols.id`. Mirrors the CLI
/// helper of the same name in `crabcc-cli::main`. Uses
/// `Store::find_by_name` (which already powers `lookup sym`); on multiple
/// hits, returns the first one — MCP callers can disambiguate by passing
/// a qualified name.
fn resolve_symbol_id(store: &Store, name: &str) -> Result<i64> {
    store
        .symbol_id_by_name(name)?
        .ok_or_else(|| anyhow::anyhow!("symbol not found: {name}"))
}
