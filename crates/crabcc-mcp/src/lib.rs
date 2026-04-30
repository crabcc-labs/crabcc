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

pub mod memory;

/// The MCP server's canonical OpenAPI 3.1 description, embedded at
/// compile time. Source of truth: `crates/crabcc-mcp/openapi.yaml`.
/// Surfaced via `crabcc openapi` (CLI) and the `_openapi` MCP tool.
pub const OPENAPI_YAML: &str = include_str!("../openapi.yaml");

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
    let mut all = tools_def_symbol();
    all.extend(memory::tools_def());
    all.extend(tools_def_meta());
    all
}

/// Meta tools — describe the server itself rather than the underlying
/// repo. Underscored prefix keeps them visually distinct from
/// repo-scoped tools and out of name-collision range with future
/// language-extractor tools.
fn tools_def_meta() -> Vec<Value> {
    vec![
        tool_schema(
            "_openapi",
            "Return the embedded OpenAPI 3.1 description of this MCP \
             server's tool surface (YAML, byte-identical to the source \
             file at crates/crabcc-mcp/openapi.yaml). Useful for SDK \
             generators and for agents that want to introspect their own \
             toolbox at runtime. Pipe through `yq -o json` if you need \
             JSON.",
            json!({}),
            &[],
        ),
        tool_schema(
            "_health",
            "Liveness + capability probe. Returns server name, semver, \
             protocol version, and the count of tools currently exposed. \
             No filesystem touches — safe to poll cheaply.",
            json!({}),
            &[],
        ),
    ]
}

// The symbol-side tools above are wrapped here so the public `tools_def()`
// can concat them with the memory-side tools without restructuring the
// existing schema literals.
fn tools_def_symbol() -> Vec<Value> {
    let mode_field = json!({
        "type": "string",
        "enum": ["hits", "files", "count"],
        "description": "Output shape. 'hits' = full {file,line,col,snippet} list (default). \
                        'files' = deduped file paths only (~70% smaller). \
                        'count' = `{count: N}` only (smallest)."
    });
    let limit_field = json!({
        "type": "integer",
        "description": "Cap result size. Omit for unlimited."
    });

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
             Coarse — matches text equality on identifier nodes. \
             Use mode='files' for 'which files reference X', mode='count' for 'how many'.",
            json!({
                "name":  str_field("symbol name"),
                "mode":  mode_field.clone(),
                "limit": limit_field.clone(),
            }),
            &["name"],
        ),
        tool_schema(
            "callers",
            "Find call sites of `name` — both bare (`foo()`) and method-receiver \
             (`obj.foo()`) shapes. Use mode='files' for 'which files call X', \
             mode='count' for 'how many calls'.",
            json!({
                "name":  str_field("function or method name"),
                "mode":  mode_field,
                "limit": limit_field,
            }),
            &["name"],
        ),
        tool_schema(
            "outline",
            "All symbols in `file` ordered by line. Use parent field to reconstruct hierarchy.",
            json!({"file": str_field("repo-relative file path")}),
            &["file"],
        ),
        tool_schema(
            "files",
            "List indexed files. Token-cheap replacement for `ls -R` / `find -name`.",
            json!({
                "under": str_field("optional path prefix to filter under"),
                "lang":  str_field("optional language filter (typescript|tsx|javascript|ruby)"),
                "ext":   str_field("optional file extension (without dot)"),
                "limit": {"type": "integer", "description": "cap output size"},
            }),
            &[],
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
        tool_schema(
            "graph",
            "Walk the call-graph sidecar (.crabcc/graph.json). Returns BFS \
             expansion of who calls (or is called by) `name`, capped at `depth`.",
            json!({
                "name":  str_field("symbol name to expand"),
                "dir":   {
                    "type": "string",
                    "enum": ["callers", "callees"],
                    "description": "Direction. 'callers' = who calls X (incoming). 'callees' = what X calls (outgoing). Default: callers.",
                },
                "depth": {"type": "integer", "description": "BFS depth limit (default 2)."},
            }),
            &["name"],
        ),
        tool_schema(
            "graph_cycles",
            "Find strongly-connected components of size ≥2 in the call-graph \
             (mutual-recursion / cycle candidates). Returns array of arrays of \
             symbol names.",
            json!({}),
            &[],
        ),
        tool_schema(
            "graph_orphans",
            "List symbols that call others but have no incoming callers. \
             Dead-code triage starting point. Returns array of names.",
            json!({}),
            &[],
        ),
        tool_schema(
            "upgrade",
            "Check GitHub for a newer crabcc release (private-repo aware via \
             local `gh` auth). Returns `{installed, latest, delta:{status,kind?}, \
             recommendations}`. Pass apply=true to also clean local sidecars \
             after the check (idempotent; user must re-index).",
            json!({
                "apply": {
                    "type": "boolean",
                    "description": "If true, rm .crabcc/{index.db,tantivy/,graph.json} after the version check. Default false.",
                },
                "repo": str_field("optional repo override (default: peterlodri-sec/crabcc)"),
            }),
            &[],
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
                "tool_count": tools_def().len(),
            });
            Ok(Some(body.to_string()))
        }
        _ => Ok(None),
    }
}

fn dispatch_tool(params: Option<&Value>, root: &Path) -> Result<String> {
    let p = params.ok_or_else(|| anyhow::anyhow!("missing params"))?;
    let tool = p
        .get("name")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing tool name"))?;
    let args = p.get("arguments").cloned().unwrap_or(json!({}));

    // Meta tools are dispatched before any filesystem work: they describe
    // the server itself (OpenAPI surface, version, tool count) and must
    // succeed even on a non-repo cwd.
    if let Some(meta) = dispatch_meta(tool, &args)? {
        return Ok(meta);
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

    fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
        args.get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing arg: {key}"))
    }

    match tool {
        "sym" => {
            let name = arg_str(&args, "name")?;
            let r = query::find_symbol(&store, name)?;
            memory::auto_capture(root, "sym", name, r.len(), &args);
            Ok(serde_json::to_string(&r)?)
        }
        "refs" => {
            let name = arg_str(&args, "name")?;
            let mode = parse_mode(&args);
            let r = query::query_refs(&store, root, name, mode)?;
            memory::auto_capture(root, "refs", name, r.count(), &args);
            Ok(serde_json::to_string(&r)?)
        }
        "callers" => {
            let name = arg_str(&args, "name")?;
            let mode = parse_mode(&args);
            let r = query::query_callers(&store, root, name, mode)?;
            memory::auto_capture(root, "callers", name, r.count(), &args);
            Ok(serde_json::to_string(&r)?)
        }
        "outline" => {
            let r = outline::outline(&store, arg_str(&args, "file")?)?;
            Ok(serde_json::to_string(&r)?)
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
            let r = index::full_index(root, &store)?;
            Ok(serde_json::to_string(&r)?)
        }
        "refresh" => {
            let r = index::refresh(root, &store)?;
            Ok(serde_json::to_string(&r)?)
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

fn load_or_build_graph(store: &Store, root: &Path) -> Result<crabcc_core::graph::CallGraph> {
    let path = root.join(".crabcc").join("graph.json");
    if path.exists() {
        crabcc_core::graph::CallGraph::load(&path)
    } else {
        crabcc_core::graph::CallGraph::build(store, root)
    }
}

fn parse_mode(args: &Value) -> query::Mode {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    match args.get("mode").and_then(|v| v.as_str()) {
        Some("count") => query::Mode::Count,
        Some("files") => query::Mode::FilesOnly { limit },
        _ => query::Mode::Hits { limit },
    }
}

fn list_indexed_files(
    store: &Store,
    under: Option<&str>,
    lang: Option<&str>,
    ext: Option<&str>,
    limit: usize,
) -> Result<Vec<String>> {
    let mut out: Vec<String> = store
        .list_files()?
        .into_iter()
        .filter(|(p, l)| {
            under.is_none_or(|u| p.starts_with(u))
                && lang.is_none_or(|want| l == want)
                && ext.is_none_or(|e| p.ends_with(&format!(".{e}")))
        })
        .map(|(p, _)| p)
        .collect();
    out.sort();
    if limit > 0 && out.len() > limit {
        out.truncate(limit);
    }
    Ok(out)
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
        assert!(resp["result"]["serverInfo"]["name"]
            .as_str()
            .unwrap()
            .contains("crabcc"));
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn handle_tools_list_has_all_tools() {
        let dir = tempfile::tempdir().unwrap();
        let req = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let resp = handle(&req, dir.path());
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in [
            "sym", "refs", "callers", "outline", "index", "refresh", "fuzzy", "prefix",
        ] {
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
        assert!(!parsed.as_array().unwrap().is_empty());
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
        assert!(
            resp["error"].is_object(),
            "expected error response, got: {resp}"
        );
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

    fn call_tool(root: &std::path::Path, tool: &str, args: Value) -> Value {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 100,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args }
        });
        handle(&req, root)
    }

    fn parse_text_content(resp: &Value) -> Value {
        let s = resp["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn tools_list_includes_memory_tools() {
        let dir = tempfile::tempdir().unwrap();
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        let resp = handle(&req, dir.path());
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in [
            "memory.init",
            "memory.remember",
            "memory.search",
            "memory.get",
            "memory.list",
            "memory.delete",
            "memory.count",
            "memory.health",
        ] {
            assert!(names.contains(&expected), "missing memory tool: {expected}");
        }
    }

    #[test]
    fn memory_remember_then_list_via_handle() {
        let dir = tempfile::tempdir().unwrap();
        // remember
        let r = call_tool(
            dir.path(),
            "memory.remember",
            json!({"source": "doc:1", "body": "hello world", "session_id": "s1"}),
        );
        let parsed = parse_text_content(&r);
        assert!(parsed["id"].as_i64().unwrap() >= 1);

        // list
        let r = call_tool(dir.path(), "memory.list", json!({}));
        let parsed = parse_text_content(&r);
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["body"], "hello world");
        assert_eq!(arr[0]["session_id"], "s1");
    }

    #[test]
    fn memory_search_via_handle() {
        let dir = tempfile::tempdir().unwrap();
        call_tool(
            dir.path(),
            "memory.remember",
            json!({"source": "1", "body": "fox jumps"}),
        );
        call_tool(
            dir.path(),
            "memory.remember",
            json!({"source": "2", "body": "cat sleeps"}),
        );
        let r = call_tool(
            dir.path(),
            "memory.search",
            json!({"query": "fox jumps", "limit": 1}),
        );
        let parsed = parse_text_content(&r);
        let hits = parsed["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["source_id"], "1");
    }

    #[test]
    fn memory_search_returns_ranked_hit_shape() {
        // Asserts the MCP tool surfaces the same ranked DrawerHit shape the
        // CLI produces (id, score, source_id, body, wing), with scores sorted
        // descending across ranking modes. Tracks issue #21.
        let dir = tempfile::tempdir().unwrap();
        for (src, body) in [
            ("a", "fox jumps over lazy dog"),
            ("b", "fox runs through forest"),
            ("c", "cat sleeps on mat"),
        ] {
            call_tool(
                dir.path(),
                "memory.remember",
                json!({"source": src, "body": body}),
            );
        }

        for mode in ["hybrid", "lexical", "vector"] {
            let r = call_tool(
                dir.path(),
                "memory.search",
                json!({"query": "fox jumps", "limit": 3, "mode": mode}),
            );
            let parsed = parse_text_content(&r);
            let hits = parsed["hits"].as_array().expect("hits is array");
            assert!(!hits.is_empty(), "{mode}: expected at least one hit");

            // Every hit must carry the full DrawerHit shape with valid types.
            for h in hits {
                assert!(h["id"].is_i64(), "{mode}: id missing/wrong type");
                assert!(h["score"].is_f64(), "{mode}: score missing/wrong type");
                assert!(h["source_id"].is_string(), "{mode}: source_id missing");
                assert!(h["body"].is_string(), "{mode}: body missing");
                assert!(h["wing"].is_string(), "{mode}: wing missing");
            }

            // Scores must be monotonically non-increasing — this is the
            // contract callers depend on for "top-K".
            let scores: Vec<f64> = hits.iter().map(|h| h["score"].as_f64().unwrap()).collect();
            assert!(
                scores.windows(2).all(|w| w[0] >= w[1]),
                "{mode}: scores not sorted desc: {scores:?}"
            );

            // Rank-1 should clearly favour the matching token over the
            // unrelated `cat sleeps` drawer.
            assert_ne!(
                hits[0]["source_id"], "c",
                "{mode}: unrelated drawer ranked first"
            );
        }
    }

    #[test]
    fn memory_search_rejects_unknown_mode() {
        let dir = tempfile::tempdir().unwrap();
        call_tool(
            dir.path(),
            "memory.remember",
            json!({"source": "1", "body": "anything"}),
        );
        let resp = call_tool(
            dir.path(),
            "memory.search",
            json!({"query": "anything", "mode": "fancy"}),
        );
        // Bad mode surfaces as a JSON-RPC error, not a silent fallback.
        assert!(resp.get("error").is_some(), "expected JSON-RPC error");
    }

    #[test]
    fn memory_count_and_delete_via_handle() {
        let dir = tempfile::tempdir().unwrap();
        call_tool(
            dir.path(),
            "memory.remember",
            json!({"source": "x", "body": "one"}),
        );
        call_tool(
            dir.path(),
            "memory.remember",
            json!({"source": "y", "body": "two"}),
        );
        let c = parse_text_content(&call_tool(dir.path(), "memory.count", json!({})));
        assert_eq!(c["count"], 2);

        let d = parse_text_content(&call_tool(
            dir.path(),
            "memory.delete",
            json!({"source": "x"}),
        ));
        assert_eq!(d["deleted"], 1);

        let c = parse_text_content(&call_tool(dir.path(), "memory.count", json!({})));
        assert_eq!(c["count"], 1);
    }

    #[test]
    fn memory_dispatch_resolves_cwd_arg_to_git_root() {
        // cwd points into a nested dir under a git root; dispatch should
        // walk up to the root and write memory.db there, not under the
        // server's startup root.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let nested = dir.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();

        let server_root = tempfile::tempdir().unwrap();
        call_tool(
            server_root.path(),
            "memory.remember",
            json!({
                "cwd": nested.display().to_string(),
                "source": "doc:1",
                "body": "hi"
            }),
        );

        // memory.db must exist under the git-root, not under server_root.
        assert!(dir.path().join(".crabcc").join("memory.db").exists());
        assert!(!server_root
            .path()
            .join(".crabcc")
            .join("memory.db")
            .exists());
    }

    #[test]
    fn memory_remember_propagates_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let r = call_tool(
            dir.path(),
            "memory.remember",
            json!({"source": "d", "body": "b", "session_id": "mcp:conv-42"}),
        );
        let id = parse_text_content(&r)["id"].as_i64().unwrap();
        let g = parse_text_content(&call_tool(dir.path(), "memory.get", json!({"id": id})));
        assert_eq!(g["session_id"], "mcp:conv-42");
    }

    #[test]
    fn memory_dispatch_health_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let r = call_tool(dir.path(), "memory.health", json!({}));
        let parsed = parse_text_content(&r);
        assert_eq!(parsed.as_str().unwrap(), "Ok");
    }

    #[test]
    fn auto_capture_inner_via_mcp_creates_drawer() {
        // Bypasses the env-var gate by calling auto_capture_inner directly.
        let dir = tempfile::tempdir().unwrap();
        memory::auto_capture_inner(dir.path(), &json!({}), "sym", "Foo", 7, Some("mcp:conv-99"));
        let r = call_tool(dir.path(), "memory.list", json!({}));
        let parsed = parse_text_content(&r);
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["session_id"], "mcp:conv-99");
        assert_eq!(arr[0]["room"], "sym");
    }

    #[test]
    fn parse_mode_default_is_hits() {
        let m = parse_mode(&json!({}));
        assert!(matches!(m, query::Mode::Hits { limit: None }));
    }

    #[test]
    fn parse_mode_count_overrides() {
        let m = parse_mode(&json!({"mode": "count"}));
        assert!(matches!(m, query::Mode::Count));
    }

    #[test]
    fn parse_mode_files_with_limit() {
        let m = parse_mode(&json!({"mode": "files", "limit": 7}));
        match m {
            query::Mode::FilesOnly { limit: Some(7) } => {}
            other => panic!("expected FilesOnly{{Some(7)}}, got: {other:?}"),
        }
    }

    #[test]
    fn parse_mode_hits_with_limit() {
        let m = parse_mode(&json!({"limit": 3}));
        match m {
            query::Mode::Hits { limit: Some(3) } => {}
            other => panic!("expected Hits{{Some(3)}}, got: {other:?}"),
        }
    }

    #[test]
    fn parse_mode_unknown_mode_falls_back_to_hits() {
        let m = parse_mode(&json!({"mode": "garbage"}));
        assert!(matches!(m, query::Mode::Hits { .. }));
    }

    #[test]
    fn handle_tools_call_refs_count_mode() {
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": { "name": "refs", "arguments": { "name": "hello", "mode": "count" } }
        });
        let resp = handle(&req, dir.path());
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert!(
            parsed.get("count").is_some(),
            "expected count field, got: {parsed}"
        );
        assert!(parsed["count"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn handle_tools_call_refs_files_only_mode() {
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": { "name": "refs", "arguments": { "name": "hello", "mode": "files" } }
        });
        let resp = handle(&req, dir.path());
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        let files = parsed["files"].as_array().expect("files field");
        assert!(files.iter().any(|v| v.as_str() == Some("hi.ts")));
    }

    #[test]
    fn handle_tools_list_includes_files_tool() {
        let dir = tempfile::tempdir().unwrap();
        let req = json!({"jsonrpc": "2.0", "id": 10, "method": "tools/list"});
        let resp = handle(&req, dir.path());
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"files"), "tools missing 'files': {names:?}");
    }

    #[test]
    fn handle_tools_call_files_filters_by_ext() {
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": { "name": "files", "arguments": { "ext": "ts" } }
        });
        let resp = handle(&req, dir.path());
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        let arr = parsed.as_array().unwrap();
        assert!(arr.iter().any(|v| v.as_str() == Some("hi.ts")));
        assert!(arr.iter().all(|v| v.as_str().unwrap().ends_with(".ts")));
    }

    #[test]
    fn handle_tools_call_files_respects_limit() {
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": { "name": "files", "arguments": { "limit": 1 } }
        });
        let resp = handle(&req, dir.path());
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 1);
    }

    #[test]
    fn handle_tools_list_includes_graph_cycles_and_orphans() {
        let dir = tempfile::tempdir().unwrap();
        let req = json!({"jsonrpc": "2.0", "id": 13, "method": "tools/list"});
        let resp = handle(&req, dir.path());
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"graph_cycles"), "tools: {names:?}");
        assert!(names.contains(&"graph_orphans"), "tools: {names:?}");
    }

    #[test]
    fn handle_tools_call_graph_orphans_returns_array() {
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 14,
            "method": "tools/call",
            "params": { "name": "graph_orphans", "arguments": {} }
        });
        let resp = handle(&req, dir.path());
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        // The fixture has hello() called from a top-level expression — so its
        // caller (None) doesn't show up; but any function with outgoing edges
        // and no callers is reported. We just check the shape.
        assert!(parsed.is_array(), "got: {parsed}");
    }

    #[test]
    fn handle_tools_call_graph_cycles_returns_array() {
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 15,
            "method": "tools/call",
            "params": { "name": "graph_cycles", "arguments": {} }
        });
        let resp = handle(&req, dir.path());
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        // The fixture has no mutual recursion, so cycles should be empty.
        assert_eq!(parsed.as_array().unwrap().len(), 0, "got: {parsed}");
    }

    // ---- OpenAPI spec drift gate ---------------------------------------
    //
    // Every tool exposed by `tools_def()` MUST have a matching
    // `operationId:` in the embedded OpenAPI spec. Conversely, every
    // operationId in the spec MUST correspond to a real tool. Either
    // direction's drift fails this test, so adding a tool without
    // updating the spec (or vice versa) blocks `task prep-pr`.

    fn tool_names_from_def() -> std::collections::BTreeSet<String> {
        // Normalise `.` → `_` so MCP tool names like `memory.search`
        // align with their OpenAPI operationId (`memory_search`). The
        // OpenAPI 3.1 spec disallows `.` in operationId values, so
        // dotted MCP names get renamed; the spec's path retains the
        // dotted form.
        tools_def()
            .iter()
            .filter_map(|t| t.get("name").and_then(|v| v.as_str()).map(String::from))
            .map(|n| n.replace('.', "_"))
            .collect()
    }

    fn operation_ids_from_spec() -> std::collections::BTreeSet<String> {
        // Hand-roll a parser. We don't depend on serde_yaml here (would
        // pull in another transitive dep tree); instead grep for lines
        // shaped exactly `      operationId: <id>` at the canonical
        // 6-space indent we use in the file. If the spec ever
        // re-formats, this regex stays trivial to update.
        OPENAPI_YAML
            .lines()
            .filter_map(|l| l.trim_start().strip_prefix("operationId:"))
            .map(|rest| rest.trim().to_string())
            .collect()
    }

    #[test]
    fn openapi_spec_lists_every_tool() {
        let in_def = tool_names_from_def();
        let in_spec = operation_ids_from_spec();
        let missing_in_spec: Vec<&String> = in_def.difference(&in_spec).collect();
        let missing_in_def: Vec<&String> = in_spec.difference(&in_def).collect();
        assert!(
            missing_in_spec.is_empty() && missing_in_def.is_empty(),
            "OpenAPI spec drift detected.\n  \
             Tools missing from openapi.yaml: {missing_in_spec:?}\n  \
             operationIds missing from tools_def(): {missing_in_def:?}\n  \
             Run `task openapi` after editing tools to refresh the spec."
        );
    }

    #[test]
    fn handle_tools_call_openapi_returns_yaml() {
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 16,
            "method": "tools/call",
            "params": { "name": "_openapi", "arguments": {} }
        });
        let resp = handle(&req, dir.path());
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        // Sanity — the embedded spec starts with `openapi: 3.1.0`.
        assert!(
            content.starts_with("openapi: 3.1.0"),
            "got: {}",
            &content[..40]
        );
    }

    #[test]
    fn handle_tools_call_health_returns_status_ok() {
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 17,
            "method": "tools/call",
            "params": { "name": "_health", "arguments": {} }
        });
        let resp = handle(&req, dir.path());
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["server"], "crabcc-mcp");
        assert_eq!(parsed["protocol_version"], "2024-11-05");
        assert!(parsed["tool_count"].as_u64().unwrap() > 0);
    }
}
