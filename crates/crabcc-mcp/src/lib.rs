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

/// Env var that flips the dev surface on at runtime — useful when the
/// caller can't pass `--dev` (e.g., when the MCP client is launched by
/// a wrapper that doesn't forward CLI flags). Mirrors `--dev`.
pub const DEV_ENV: &str = "CRABCC_MCP_DEV";

/// True when the dev surface should be exposed — checked once per
/// `serve_stdio` start so flipping the env mid-session has no effect.
pub fn dev_mode_from_env() -> bool {
    std::env::var(DEV_ENV).ok().as_deref() == Some("1")
}

pub fn serve_stdio(root: &Path) -> Result<()> {
    serve_stdio_with(root, dev_mode_from_env())
}

/// Same as [`serve_stdio`] but takes the dev flag explicitly. Used by
/// the CLI's `--dev` plumbing and by tests that want to exercise both
/// surfaces independently of process env.
pub fn serve_stdio_with(root: &Path, dev: bool) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());
    let writer = stdout.lock();
    serve_io(reader, writer, root, dev)
}

/// Generic I/O variant — issue #89 slice 1.
///
/// Drives the JSON-RPC loop against any [`BufRead`] / [`Write`] pair.
/// `serve_stdio_with` wraps locked stdin/stdout; tests pipe a
/// `Cursor<Vec<u8>>` of newline-delimited JSON in and capture the
/// response stream on a `Vec<u8>` writer — no `tempfile`, no pipe,
/// no subprocess.
///
/// # Hot-path discipline
///
/// Three changes vs the obvious `read_line` / `writeln!("{}")` form:
///
/// 1. **`read_until(b'\n')` + `from_slice`** — skips the UTF-8
///    validation pass `read_line` does on every byte. serde_json's
///    parser does its own UTF-8 check on the strings it cares about,
///    so the upfront pass is duplicate work.
/// 2. **`to_writer` + `write_all(b"\n")`** — replaces
///    `writeln!(writer, "{value}")`, which goes through `Display` →
///    `Value::to_string()` and allocates an intermediate `String`
///    per response. The new form serialises directly into the
///    writer's buffer.
/// 3. **One reusable `Vec<u8>`** — `clear()` keeps the capacity, so
///    subsequent requests don't re-allocate after the first big-ish
///    one. Pre-sized 4 KiB to cover the common case (most MCP
///    requests fit in one TCP segment).
///
/// Net effect: zero `String` allocations on the steady-state path
/// (notifications + responses both); one `Vec<u8>` grow at most.
pub fn serve_io<R, W>(mut reader: R, mut writer: W, root: &Path, dev: bool) -> Result<()>
where
    R: BufRead,
    W: Write,
{
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
        // Skip empty / whitespace-only frames without going through
        // String::trim — bytes-only check, no UTF-8 validation.
        if buf.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        // serde_json::from_slice tolerates leading whitespace per RFC
        // 7159, so the bytes-only frame check above is the only
        // pre-parse work needed.
        let req: Value = match serde_json::from_slice(&buf) {
            Ok(v) => v,
            Err(e) => {
                let resp = error_response(None, -32700, &format!("parse error: {e}"));
                serde_json::to_writer(&mut writer, &resp)?;
                writer.write_all(b"\n")?;
                writer.flush()?;
                continue;
            }
        };
        let resp = handle_with(&req, root, dev);
        // Spec: notifications get no response. Skip empty/Null
        // (notifications/initialized in particular).
        if resp.is_null() {
            continue;
        }
        serde_json::to_writer(&mut writer, &resp)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
}

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

/// Default tool surface — equivalent to `tools_def_for(dev_mode_from_env())`.
/// Existing callers (notably the OpenAPI drift test) keep working unchanged.
pub fn tools_def() -> Vec<Value> {
    tools_def_for(dev_mode_from_env())
}

/// Tool surface gated on the dev flag.
///
/// **Default (dev=false)** — agent-facing surface. Drops the meta
/// tools (`_openapi`, `_health`) which are diagnostic-only and
/// unhelpful for normal queries. Closes #59 for the meta surface.
///
/// **Dev (dev=true)** — full surface, identical to pre-#59 behaviour.
/// Use when generating SDK bindings, when tooling needs the OpenAPI
/// dump, or when a CI matrix wants to drift-check the full schema.
pub fn tools_def_for(dev: bool) -> Vec<Value> {
    let mut all = tools_def_symbol();
    all.extend(memory::tools_def());
    if dev {
        all.extend(tools_def_meta());
    }
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
        "enum": ["hits", "files", "summary", "count"],
        "description": "Output shape. 'hits' = full {file,line,col,snippet} list (default). \
                        'files' = deduped file paths only (~70% smaller). \
                        'summary' = `{by_file: {path: N, ...}}` distribution (~95% smaller than hits). \
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
             {name, kind, signature, parent, file, line_start, line_end, visibility}. \
             Pass `since` to restrict to files changed since a git revision.",
            json!({
                "name":  str_field("symbol name to look up"),
                "since": str_field(
                    "Optional git revision (SHA / ref / `HEAD~N`). Restricts \
                     results to files changed in `<since>...HEAD`."
                ),
            }),
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
                "if_changed": str_field(
                    "Cache-revalidation hint. Pass the fingerprint from a \
                     previous call; on match the response collapses to \
                     {unchanged: true, fingerprint: ...}. On mismatch the \
                     full result is wrapped as {fingerprint, result}."
                ),
                "since": str_field(
                    "Optional git revision. Restricts results to files \
                     changed in `<since>...HEAD`."
                ),
                "stream": {
                    "type": "boolean",
                    "description": "When true, emit NDJSON (one hit object \
                                    per line) instead of a single JSON \
                                    array. Hits-mode only — combining with \
                                    `mode=count|files|summary` errors."
                },
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
                "if_changed": str_field(
                    "Cache-revalidation hint — see `refs.if_changed`."
                ),
                "since": str_field(
                    "Optional git revision — see `refs.since`."
                ),
                "stream": {
                    "type": "boolean",
                    "description": "NDJSON stream — see `refs.stream`."
                },
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
            "Incremental refresh: mtime + sha256 diff vs stored. Pass \
             `delta: true` to receive `{added, modified, removed, stats}` \
             instead of bare counts so the caller knows exactly which \
             files to re-read.",
            json!({
                "delta": {
                    "type": "boolean",
                    "description": "Include per-bucket file lists in the response."
                }
            }),
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

fn load_or_build_graph(store: &Store, root: &Path) -> Result<crabcc_core::graph::CallGraph> {
    let path = root.join(".crabcc").join("graph.json");
    if path.exists() {
        crabcc_core::graph::CallGraph::load(&path)
    } else {
        crabcc_core::graph::CallGraph::build(store, root)
    }
}

/// Resolve the optional `since` MCP arg to a changed-files set via
/// `gitdiff::changed_files_since`. Returns `Ok(None)` when the arg is
/// absent so callers can use `Option::as_ref()` to drive the filter
/// path. A bad git revision surfaces as a tool error per JSON-RPC.
fn since_filter(args: &Value, root: &Path) -> Result<Option<std::collections::HashSet<String>>> {
    match args.get("since").and_then(|v| v.as_str()) {
        Some(rev) => Ok(Some(crabcc_core::gitdiff::changed_files_since(root, rev)?)),
        None => Ok(None),
    }
}

/// True when the caller asked for NDJSON streaming (one hit per line).
/// Distinct from `if_changed` and the existing JSON envelope — those are
/// mutually exclusive at the call site (the CLI flag rejects the combo;
/// the MCP path just prefers stream when both are set, since the
/// fingerprint envelope only makes sense over a single JSON blob).
fn want_stream(args: &Value) -> bool {
    args.get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Serialize an `Output::Hits` payload as newline-delimited JSON — one
/// hit object per line. Other output shapes are not streamable; we
/// surface those as a tool error so the caller can switch shapes.
fn hits_to_ndjson(out: &query::Output) -> Result<String> {
    let hits = match out {
        query::Output::Hits(h) => h,
        _ => {
            return Err(anyhow::anyhow!(
                "stream=true requires hits-mode output (got non-Hits shape)"
            ));
        }
    };
    let mut buf = String::new();
    for h in hits {
        buf.push_str(&serde_json::to_string(h)?);
        buf.push('\n');
    }
    Ok(buf)
}

fn parse_mode(args: &Value) -> query::Mode {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    match args.get("mode").and_then(|v| v.as_str()) {
        Some("count") => query::Mode::Count,
        Some("files") => query::Mode::FilesOnly { limit },
        Some("summary") => query::Mode::Summary { limit },
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
            "memory.mine_project",
            "memory.mine_sessions",
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
    fn memory_forget_by_drawer_id() {
        let dir = tempfile::tempdir().unwrap();
        let r = call_tool(
            dir.path(),
            "memory.remember",
            json!({"source": "doc:1", "body": "to drop"}),
        );
        let id = parse_text_content(&r)["id"].as_i64().unwrap();

        let f = parse_text_content(&call_tool(
            dir.path(),
            "memory.forget",
            json!({"drawer": id}),
        ));
        assert_eq!(f["forgotten"], 1);

        // Forgetting it again is a no-op (issue #26 idempotency contract).
        let again = parse_text_content(&call_tool(
            dir.path(),
            "memory.forget",
            json!({"drawer": id}),
        ));
        assert_eq!(again["forgotten"], 0);
    }

    #[test]
    fn memory_forget_rejects_invalid_arg_combos() {
        let dir = tempfile::tempdir().unwrap();
        // Neither selector → JSON-RPC error, not a silent fallback that
        // wipes the store.
        let resp = call_tool(dir.path(), "memory.forget", json!({}));
        assert!(resp.get("error").is_some(), "no selector must error");

        // Mixing selectors is also rejected.
        let resp = call_tool(
            dir.path(),
            "memory.forget",
            json!({"drawer": 1, "wing": "w", "before": "100"}),
        );
        assert!(resp.get("error").is_some(), "mixed selectors must error");

        // Wing without before is rejected.
        let resp = call_tool(dir.path(), "memory.forget", json!({"wing": "w"}));
        assert!(
            resp.get("error").is_some(),
            "wing without before must error"
        );
    }

    #[test]
    fn memory_forget_accepts_rfc3339_before() {
        // Smoke test of the MCP-side RFC3339 path — the actual cutoff
        // logic is exercised via Palace tests; here we just confirm the
        // tool dispatches without error and reports a forgotten count.
        let dir = tempfile::tempdir().unwrap();
        call_tool(
            dir.path(),
            "memory.remember",
            json!({"source": "doc:1", "body": "x", "wing": "notes"}),
        );
        let resp = parse_text_content(&call_tool(
            dir.path(),
            "memory.forget",
            // Far-future cutoff → drops the freshly-inserted row.
            json!({"wing": "notes", "before": "2099-01-01T00:00:00Z"}),
        ));
        assert_eq!(resp["forgotten"], 1);
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
    fn memory_mine_project_via_handle() {
        // Build a tiny synthetic repo, point the tool at it, and confirm
        // the report says two drawers landed under wing="proj".
        let server_root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::write(target.path().join("notes.md"), "alpha beta gamma").unwrap();
        std::fs::write(target.path().join("readme.txt"), "the quick brown fox").unwrap();

        let r = call_tool(
            server_root.path(),
            "memory.mine_project",
            json!({"path": target.path().display().to_string()}),
        );
        let report = parse_text_content(&r);
        assert_eq!(report["inserted"], 2, "report shape: {report}");

        // Second call → all dedup hits, no new rows.
        let r2 = call_tool(
            server_root.path(),
            "memory.mine_project",
            json!({"path": target.path().display().to_string()}),
        );
        let report2 = parse_text_content(&r2);
        assert_eq!(report2["inserted"], 0);
        assert_eq!(report2["deduped"], 2);
    }

    #[test]
    fn memory_mine_sessions_via_handle() {
        let server_root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let f = target.path().join("conv.jsonl");
        let body = concat!(
            r#"{"message":{"role":"user","content":"what about plums?"}}"#,
            "\n",
            r#"{"message":{"role":"assistant","content":"plums need cool nights"}}"#,
            "\n",
        );
        std::fs::write(&f, body).unwrap();

        let r = call_tool(
            server_root.path(),
            "memory.mine_sessions",
            json!({"dir": target.path().display().to_string()}),
        );
        let report = parse_text_content(&r);
        assert_eq!(report["inserted"], 1);
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
    fn handle_tools_call_refs_stream_emits_ndjson() {
        // `stream: true` → response body is NDJSON (one hit per line),
        // not a JSON array. Each line must be valid JSON on its own.
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 70,
            "method": "tools/call",
            "params": { "name": "refs", "arguments": { "name": "hello", "stream": true } }
        });
        let resp = handle(&req, dir.path());
        let body = resp["result"]["content"][0]["text"].as_str().unwrap();
        let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
        assert!(!lines.is_empty(), "expected at least one NDJSON line");
        for line in &lines {
            // Each line must parse as a Hit object.
            let v: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("invalid NDJSON line {line:?}: {e}"));
            assert!(v["file"].is_string());
            assert!(v["line"].is_number());
        }
    }

    #[test]
    fn handle_tools_call_refs_stream_with_count_mode_errors() {
        // stream=true requires hits-mode. Other modes should return a
        // JSON-RPC tool error rather than producing a malformed response.
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 71,
            "method": "tools/call",
            "params": {
                "name": "refs",
                "arguments": { "name": "hello", "stream": true, "mode": "count" }
            }
        });
        let resp = handle(&req, dir.path());
        assert!(
            resp.get("error").is_some(),
            "expected JSON-RPC error, got: {resp}"
        );
    }

    #[test]
    fn handle_tools_call_sym_since_filters_to_changed_files() {
        // sym with a `since` arg pointing at HEAD (no diff) should
        // return zero results because no files have changed in the window.
        // Setup: create a real git repo so `git diff` has something to
        // resolve against.
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["-C", &dir.path().display().to_string(), "init", "-q"])
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-C",
                &dir.path().display().to_string(),
                "config",
                "user.email",
                "t@t",
            ])
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-C",
                &dir.path().display().to_string(),
                "config",
                "user.name",
                "t",
            ])
            .status()
            .unwrap();
        std::fs::write(
            dir.path().join("hi.ts"),
            "export function hello(name: string){return name;}\n",
        )
        .unwrap();
        std::process::Command::new("git")
            .args(["-C", &dir.path().display().to_string(), "add", "-A"])
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-C",
                &dir.path().display().to_string(),
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "init",
            ])
            .status()
            .unwrap();
        std::fs::create_dir_all(dir.path().join(".crabcc")).unwrap();
        let store = Store::open(&dir.path().join(".crabcc").join("index.db")).unwrap();
        crabcc_core::index::full_index(dir.path(), &store).unwrap();

        // since=HEAD => no diff against HEAD => empty changed-files set => zero hits.
        let req = json!({
            "jsonrpc": "2.0",
            "id": 72,
            "method": "tools/call",
            "params": { "name": "sym", "arguments": { "name": "hello", "since": "HEAD" } }
        });
        let resp = handle(&req, dir.path());
        let body = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(body).unwrap();
        let arr = parsed.as_array().unwrap();
        assert!(
            arr.is_empty(),
            "expected zero hits with since=HEAD, got: {arr:?}"
        );
    }

    #[test]
    fn handle_tools_call_refresh_delta_returns_file_lists() {
        // First call after `full_index` should be a no-op (everything
        // unchanged). Then add a new file and verify it shows up under
        // `added` in the delta response.
        let dir = fixture_root();
        let req_noop = json!({
            "jsonrpc": "2.0",
            "id": 80,
            "method": "tools/call",
            "params": { "name": "refresh", "arguments": { "delta": true } }
        });
        let resp = handle(&req_noop, dir.path());
        let body = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(body).unwrap();
        assert!(parsed["added"].as_array().unwrap().is_empty());
        assert!(parsed["modified"].as_array().unwrap().is_empty());
        assert!(parsed["removed"].as_array().unwrap().is_empty());
        assert!(parsed["stats"].is_object());

        // Add a new file and re-call with delta=true.
        std::fs::write(
            dir.path().join("added.ts"),
            "export function added(){return 7;}",
        )
        .unwrap();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 81,
            "method": "tools/call",
            "params": { "name": "refresh", "arguments": { "delta": true } }
        });
        let resp = handle(&req, dir.path());
        let body = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(body).unwrap();
        let added: Vec<String> = parsed["added"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(added, vec!["added.ts".to_string()]);
    }

    #[test]
    fn handle_tools_call_refresh_without_delta_returns_stats_only() {
        // Default shape (no `delta` arg) must still be just RefreshStats —
        // backwards-compat with callers built before this feature landed.
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 82,
            "method": "tools/call",
            "params": { "name": "refresh", "arguments": {} }
        });
        let resp = handle(&req, dir.path());
        let body = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(body).unwrap();
        // Stats fields present, delta lists absent.
        assert!(parsed.get("unchanged").is_some());
        assert!(parsed.get("added").is_none());
        assert!(parsed.get("modified").is_none());
    }

    #[test]
    fn handle_tools_call_refs_if_changed_round_trip() {
        // First call with no `if_changed` returns the result body verbatim.
        // Agent computes the fingerprint and passes it back on the second
        // call; the response collapses to the unchanged sentinel.
        let dir = fixture_root();
        let first = json!({
            "jsonrpc": "2.0",
            "id": 90,
            "method": "tools/call",
            "params": { "name": "refs", "arguments": { "name": "hello", "mode": "count" } }
        });
        let resp1 = handle(&first, dir.path());
        let body1 = resp1["result"]["content"][0]["text"].as_str().unwrap();

        let fp = crabcc_core::hash::sha256_hex(body1.as_bytes());
        let second = json!({
            "jsonrpc": "2.0",
            "id": 91,
            "method": "tools/call",
            "params": {
                "name": "refs",
                "arguments": { "name": "hello", "mode": "count", "if_changed": fp }
            }
        });
        let resp2 = handle(&second, dir.path());
        let body2 = resp2["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(body2).unwrap();
        assert_eq!(parsed["unchanged"], true);
        assert_eq!(parsed["fingerprint"], fp);

        // And a stale fingerprint produces the wrap-with-fresh-fp shape.
        let stale = "0".repeat(64);
        let third = json!({
            "jsonrpc": "2.0",
            "id": 92,
            "method": "tools/call",
            "params": {
                "name": "refs",
                "arguments": { "name": "hello", "mode": "count", "if_changed": stale }
            }
        });
        let resp3 = handle(&third, dir.path());
        let body3 = resp3["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(body3).unwrap();
        assert!(parsed.get("fingerprint").is_some());
        assert!(parsed.get("result").is_some());
        assert_ne!(parsed["fingerprint"], stale);
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
        //
        // Use the dev surface (`tools_def_for(true)`) since the OpenAPI
        // spec is the canonical *full* tool surface — dev-only tools
        // (`_openapi`, `_health`) appear in the spec, and the drift
        // test asserts both lists agree. Issue #59 hides those from
        // the *default* surface but doesn't drop them from the spec.
        tools_def_for(true)
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
        // Issue #59 — meta tools are dev-only. Use `handle_with(dev=true)`.
        let resp = handle_with(&req, dir.path(), true);
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
        let resp = handle_with(&req, dir.path(), true);
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["server"], "crabcc-mcp");
        assert_eq!(parsed["protocol_version"], "2024-11-05");
        assert!(parsed["tool_count"].as_u64().unwrap() > 0);
    }

    // ---- issue #59 — dev gate -----------------------------------------------

    #[test]
    fn dev_gate_default_surface_hides_meta_tools() {
        let dir = tempfile::tempdir().unwrap();
        let req = json!({"jsonrpc": "2.0", "id": 50, "method": "tools/list"});
        let resp = handle_with(&req, dir.path(), false);
        let names: Vec<&str> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(
            !names.contains(&"_openapi"),
            "default surface must hide _openapi"
        );
        assert!(
            !names.contains(&"_health"),
            "default surface must hide _health"
        );
        // Sanity — the agent-facing tools must still be there.
        for must_have in ["sym", "refs", "callers", "outline", "memory.search"] {
            assert!(names.contains(&must_have), "missing: {must_have}");
        }
    }

    #[test]
    fn dev_gate_dev_surface_exposes_meta_tools() {
        let dir = tempfile::tempdir().unwrap();
        let req = json!({"jsonrpc": "2.0", "id": 51, "method": "tools/list"});
        let resp = handle_with(&req, dir.path(), true);
        let names: Vec<&str> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(
            names.contains(&"_openapi"),
            "dev surface must list _openapi"
        );
        assert!(names.contains(&"_health"), "dev surface must list _health");
    }

    #[test]
    fn dev_gate_default_dispatch_rejects_meta_call() {
        // tools/list hides _openapi, but a misbehaving caller might
        // still invoke it. The dispatch path must refuse with a clear
        // error pointing at `--dev` / `CRABCC_MCP_DEV`.
        let dir = tempfile::tempdir().unwrap();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 52,
            "method": "tools/call",
            "params": { "name": "_openapi", "arguments": {} }
        });
        let resp = handle_with(&req, dir.path(), false);
        assert!(
            resp.get("error").is_some(),
            "expected JSON-RPC error, got: {resp}"
        );
        let msg = resp["error"]["message"].as_str().unwrap_or("");
        assert!(
            msg.contains("dev-only") || msg.contains("--dev"),
            "error must hint at the dev flag: {msg}"
        );
    }

    #[test]
    fn dev_gate_default_surface_is_smaller() {
        // Concrete count assertion — locks in the issue #59 win so a
        // future PR can't accidentally reintroduce a meta tool to the
        // default surface and shrink the savings.
        let default_count = tools_def_for(false).len();
        let dev_count = tools_def_for(true).len();
        assert!(
            default_count + 2 == dev_count,
            "expected default = dev - 2 (drop _openapi + _health), got default={default_count}, dev={dev_count}"
        );
    }

    // ───────────────────────────────────────────────────────────────────
    // Tool-coverage sweep (issue #18). One happy-path assertion per
    // dispatched tool that the original test suite did not cover, plus
    // a single description-shape test that gates "every tool advertises
    // a non-empty description".
    // ───────────────────────────────────────────────────────────────────

    /// Every advertised tool — default + dev surface — must carry a
    /// `description` of at least 12 characters. Catches accidental
    /// empty-string descriptions in new tool definitions. 12 is below
    /// the shortest existing description so this won't false-flag.
    #[test]
    fn every_tool_advertises_a_description() {
        for tool in tools_def_for(true) {
            let name = tool["name"].as_str().unwrap_or("(no-name)");
            let desc = tool["description"].as_str().unwrap_or("");
            assert!(
                desc.len() >= 12,
                "tool {name:?} description too short or missing: {desc:?}"
            );
        }
    }

    /// Every tool must declare an `inputSchema.properties` object — even
    /// the no-arg meta tools, which advertise an empty `{}`. Without
    /// this MCP clients can't introspect the call shape.
    #[test]
    fn every_tool_carries_input_schema() {
        for tool in tools_def_for(true) {
            let name = tool["name"].as_str().unwrap_or("(no-name)");
            assert!(
                tool["inputSchema"]["type"] == "object",
                "tool {name:?} inputSchema.type must be 'object'"
            );
            assert!(
                tool["inputSchema"]["properties"].is_object(),
                "tool {name:?} inputSchema.properties must be an object"
            );
        }
    }

    #[test]
    fn handle_tools_call_refs_returns_hits_envelope() {
        let dir = fixture_root();
        let r = call_tool(dir.path(), "refs", json!({"name": "hello"}));
        let parsed = parse_text_content(&r);
        // Default mode is Hits; the JSON has fingerprint envelope keys
        // (data + sha) — assert one of the recognisable shapes.
        let raw_text = r["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            raw_text.contains("hello") || parsed["data"].is_array() || parsed.is_array(),
            "refs payload should mention `hello`: {raw_text:.200}"
        );
    }

    #[test]
    fn handle_tools_call_callers_returns_envelope() {
        let dir = fixture_root();
        let r = call_tool(dir.path(), "callers", json!({"name": "hello"}));
        let raw_text = r["result"]["content"][0]["text"].as_str().unwrap_or("");
        // Either the fingerprint envelope or a streamed shape — both
        // count "hello" as a callable.
        assert!(
            raw_text.contains("\"") && !raw_text.is_empty(),
            "callers payload must be JSON: {raw_text:.200}"
        );
    }

    #[test]
    fn handle_tools_call_files_lists_indexed_paths() {
        let dir = fixture_root();
        let r = call_tool(dir.path(), "files", json!({}));
        let parsed = parse_text_content(&r);
        let arr = parsed.as_array().expect("files must return a JSON array");
        assert!(
            arr.iter().any(|p| p.as_str() == Some("hi.ts")),
            "files must include the fixture's hi.ts: {arr:?}"
        );
    }

    #[test]
    fn handle_tools_call_index_default_returns_stats() {
        let dir = fixture_root();
        let r = call_tool(dir.path(), "index", json!({}));
        let parsed = parse_text_content(&r);
        // Bare-stats shape: top-level keys, no envelope.
        assert!(
            parsed["files_indexed"].is_u64(),
            "index default must return IndexStats: {parsed:?}"
        );
    }

    #[test]
    fn handle_tools_call_index_with_logs_envelope() {
        // PR #101 added an opt-in `logs: true` arg to the `index` tool.
        // Verifies the alternate response shape — the in-process path
        // returns an empty `logs` array (in-process tracing isn't piped
        // through), but the envelope keys must be present so /live and
        // MCP clients consume identical JSON.
        let dir = fixture_root();
        let r = call_tool(dir.path(), "index", json!({"logs": true}));
        let parsed = parse_text_content(&r);
        assert!(parsed["stats"].is_object(), "missing stats: {parsed}");
        assert!(
            parsed["elapsed_ms"].is_u64(),
            "missing elapsed_ms: {parsed}"
        );
        assert!(parsed["logs"].is_array(), "missing logs array: {parsed}");
    }

    #[test]
    fn handle_tools_call_refresh_returns_stats() {
        let dir = fixture_root();
        let r = call_tool(dir.path(), "refresh", json!({}));
        let parsed = parse_text_content(&r);
        // RefreshStats has `unchanged` field; on a freshly indexed repo
        // every file should land in that bucket.
        assert!(
            parsed["unchanged"].is_u64() || parsed.get("stats").is_some(),
            "refresh must return RefreshStats: {parsed:?}"
        );
    }

    #[test]
    fn handle_tools_call_refresh_delta_returns_per_bucket_lists() {
        let dir = fixture_root();
        let r = call_tool(dir.path(), "refresh", json!({"delta": true}));
        let parsed = parse_text_content(&r);
        assert!(parsed["added"].is_array(), "missing added bucket: {parsed}");
        assert!(parsed["modified"].is_array(), "missing modified: {parsed}");
        assert!(parsed["removed"].is_array(), "missing removed: {parsed}");
        assert!(parsed["stats"].is_object(), "missing stats: {parsed}");
    }

    #[test]
    fn handle_tools_call_fuzzy_returns_array_shape() {
        // The fixture's `full_index` populates SQLite but not Tantivy
        // (the FTS sidecar is built explicitly via `Fts::rebuild`).
        // For the dispatcher contract test, we only assert shape — a
        // separate end-to-end test in the fts module covers retrieval
        // correctness on a real Tantivy index.
        let dir = fixture_root();
        let r = call_tool(dir.path(), "fuzzy", json!({"query": "helo"}));
        let parsed = parse_text_content(&r);
        assert!(parsed.is_array(), "fuzzy must return JSON array: {parsed}");
    }

    #[test]
    fn handle_tools_call_prefix_returns_array_shape() {
        let dir = fixture_root();
        let r = call_tool(dir.path(), "prefix", json!({"query": "hel"}));
        let parsed = parse_text_content(&r);
        assert!(parsed.is_array(), "prefix must return JSON array: {parsed}");
    }

    #[test]
    fn handle_tools_call_graph_walk_returns_hits_array() {
        let dir = fixture_root();
        let r = call_tool(
            dir.path(),
            "graph",
            json!({"name": "hello", "dir": "callers", "depth": 2}),
        );
        let parsed = parse_text_content(&r);
        // graph walk returns `Vec<GraphHit>` — empty is fine for the
        // single-file fixture; the contract is "JSON array, no error".
        assert!(parsed.is_array(), "graph walk must be JSON array: {parsed}");
    }

    /// Required-arg validation: tools that take a `name` arg must error
    /// when it's missing, with a message that mentions the missing key.
    /// Fast-fail on the dispatch side before any DB work happens.
    #[test]
    fn handle_required_args_validated_for_name_tools() {
        let dir = fixture_root();
        for tool in ["sym", "refs", "callers", "fuzzy", "prefix"] {
            let req = json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": tool, "arguments": {}}
            });
            let resp = handle(&req, dir.path());
            assert!(
                resp.get("error").is_some(),
                "{tool} with no args must error: {resp}"
            );
            let msg = resp["error"]["message"].as_str().unwrap_or("");
            assert!(
                msg.contains("name") || msg.contains("query") || msg.contains("missing"),
                "{tool} error must mention the missing arg: {msg}"
            );
        }
    }

    /// Outline must error when `file` is missing.
    #[test]
    fn handle_outline_requires_file_arg() {
        let dir = fixture_root();
        let req = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "outline", "arguments": {}}
        });
        let resp = handle(&req, dir.path());
        assert!(resp.get("error").is_some(), "outline must require file");
    }

    /// Memory tool descriptions must mention a domain concept — guards
    /// against a future copy/paste accidentally shipping a generic
    /// placeholder description on a memory tool. The keyword set covers
    /// the vocabulary actually used by current `memory.*` definitions.
    #[test]
    fn memory_tool_descriptions_are_memory_specific() {
        let keywords = [
            "memory",
            "drawer",
            "wing",
            "session",
            "vacuum",
            "idempotent",
            "hybrid",
            "bm25",
            "health",
            "store",
            "ok / degraded",
            "transcript",
            "jsonl",
        ];
        for tool in tools_def_for(false) {
            let name = tool["name"].as_str().unwrap_or("");
            if !name.starts_with("memory.") {
                continue;
            }
            let desc = tool["description"].as_str().unwrap_or("").to_lowercase();
            assert!(
                keywords.iter().any(|kw| desc.contains(kw)),
                "memory tool {name:?} description should mention a domain concept: {desc:?}"
            );
        }
    }

    // =========================================================================
    // serve_io tests — issue #89 slice 1.
    // Drive the JSON-RPC loop against in-memory readers/writers so we can
    // assert response framing without `tempfile` / pipes / subprocess.
    // =========================================================================

    use std::io::Cursor;

    /// Send `requests` (one JSON-RPC object per element), get back the
    /// newline-delimited responses as parsed Values.
    fn drive_serve_io(requests: &[Value], dev: bool) -> Vec<Value> {
        let mut input = String::new();
        for r in requests {
            input.push_str(&r.to_string());
            input.push('\n');
        }
        let reader = Cursor::new(input);
        let mut writer: Vec<u8> = Vec::new();
        // root doesn't matter for these tests — we exercise the framing,
        // not the tool implementations (those have their own tests).
        let root = std::env::temp_dir();
        super::serve_io(reader, &mut writer, &root, dev).expect("serve_io ok");
        let body = String::from_utf8(writer).expect("response is utf-8");
        body.lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).expect("line is JSON"))
            .collect()
    }

    #[test]
    fn serve_io_handles_initialize_then_tools_list() {
        let resps = drive_serve_io(
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
            ],
            false,
        );
        assert_eq!(resps.len(), 2, "expected one response per request");
        assert_eq!(resps[0]["id"], 1);
        assert_eq!(resps[0]["result"]["serverInfo"]["name"], "crabcc-mcp");
        assert_eq!(resps[1]["id"], 2);
        let tools = resps[1]["result"]["tools"].as_array().unwrap();
        assert!(!tools.is_empty(), "tools/list should return ≥ 1 tool");
    }

    #[test]
    fn serve_io_skips_response_for_notifications() {
        // notifications/initialized has no `id` and expects no response per
        // the JSON-RPC spec. Mixing it between two real requests must NOT
        // shift the response indices.
        let resps = drive_serve_io(
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
            ],
            false,
        );
        assert_eq!(resps.len(), 2, "notification must not produce a frame");
        assert_eq!(resps[0]["id"], 1);
        assert_eq!(resps[1]["id"], 2);
    }

    #[test]
    fn serve_io_handles_non_utf8_bytes_gracefully() {
        // The optimization that swapped read_line→read_until means we no
        // longer validate UTF-8 upfront — invalid bytes reach serde_json
        // and surface as a parse error, not a panic / silent drop.
        // Spec-correct: don't crash on malformed input.
        let mut input = Vec::new();
        input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":1,");
        input.push(0xFF); // lone continuation byte → invalid UTF-8
        input.extend_from_slice(b"\"method\":\"initialize\"}\n");
        input.extend_from_slice(br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        input.push(b'\n');
        let mut writer: Vec<u8> = Vec::new();
        super::serve_io(
            Cursor::new(input),
            &mut writer,
            &std::env::temp_dir(),
            false,
        )
        .unwrap();
        let frames: Vec<Value> = writer
            .split(|b| *b == b'\n')
            .filter(|s| !s.is_empty())
            .filter_map(|s| serde_json::from_slice(s).ok())
            .collect();
        assert_eq!(frames.len(), 2, "loop must keep going past invalid UTF-8");
        assert_eq!(frames[0]["error"]["code"], -32700);
        assert_eq!(frames[1]["id"], 2);
    }

    #[test]
    fn serve_io_returns_parse_error_for_malformed_line() {
        // Bad JSON between two valid frames. Loop must keep going + emit
        // a -32700 parse error for the bad line.
        let mut input = String::new();
        input.push_str(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"}).to_string());
        input.push('\n');
        input.push_str("{ not valid json\n");
        input.push_str(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string());
        input.push('\n');
        let mut writer: Vec<u8> = Vec::new();
        super::serve_io(
            Cursor::new(input),
            &mut writer,
            &std::env::temp_dir(),
            false,
        )
        .unwrap();
        let lines: Vec<Value> = String::from_utf8(writer)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["id"], 1);
        assert_eq!(lines[1]["error"]["code"], -32700);
        assert_eq!(lines[2]["id"], 2);
    }
}
