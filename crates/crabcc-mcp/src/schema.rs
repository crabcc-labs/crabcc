//! Tool catalog + JSON schema builders.
//!
//! `tools_def_for(dev)` is the entry point used by the `tools/list`
//! JSON-RPC method in [`super::dispatch::handle_with`]. The symbol-side
//! schema is hand-coded here; the memory-side comes from
//! [`super::memory::tools_def`].
//!
//! The schema-builder helpers (`arg_str`, `str_field`, `tool_schema`)
//! are `pub(crate)` so `memory.rs` can build its own tool defs against
//! the same shape.

use crate::{dev_mode_from_env, memory};
use anyhow::Result;
use serde_json::{json, Value};

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
            "test_context",
            "Bundle every piece of context an LLM needs to generate a unit \
             test for a symbol. Single round-trip replacement for the LSPRAG \
             fan-out: definition + signature + body excerpt, all call sites \
             (callers), all references, the file's outline, and the symbol's \
             dependency blast radius (transitive callees from the edge graph). \
             Pass `name` (required) and optionally `file` to disambiguate when \
             a name resolves in multiple files. Returns a JSON object with \
             keys: symbol, callers, refs, outline, blast_radius.",
            json!({
                "name": str_field("symbol name (required)"),
                "file": str_field("repo-relative file path to disambiguate (optional)"),
                "max_callers":  { "type": "integer", "description": "cap on callers returned (default 50)" },
                "max_refs":     { "type": "integer", "description": "cap on refs returned (default 50)" },
                "blast_depth":  { "type": "integer", "description": "BFS depth for transitive callees (default 2)" },
            }),
            &["name"],
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
        tool_schema(
            "read",
            "Outline-stub-aware file read. Cache key: (path, session_id) in \
             memory.db's session_reads table. First read in a session: full \
             content. Subsequent reads on same (path, session_id): outline \
             stub (~30x cheaper) — unless mtime or content_hash drift, which \
             invalidates the cache. Modes: `auto` (default) | `full` | `stub` \
             | `entropy`. `entropy` filters lines below `threshold` bits/char \
             (default 2.5) — useful for log tails / generated bundles.",
            json!({
                "path": str_field("repo-relative or absolute file path"),
                "mode": {
                    "type": "string",
                    "enum": ["auto", "full", "stub", "entropy"],
                    "description": "Read mode. Default: auto."
                },
                "session_id": str_field(
                    "Cache scope. When omitted, reads $CRABCC_SESSION_ID; \
                     when both empty, caching is bypassed and full content \
                     is always returned."
                ),
                "threshold": {
                    "type": "number",
                    "description": "Shannon-entropy threshold (bits/char) for \
                                    `mode=entropy`. Default 2.5."
                },
            }),
            &["path"],
        ),
        tool_schema(
            "ctx",
            "Meta-tool: dispatch any other crabcc tool by name. Lets the \
             agent call `ctx(tool='sym', args={name: 'Foo'})` instead of \
             tracking 25 separate tool definitions. The model sees `ctx` + a \
             handful of named tools and can compose calls dynamically. \
             Equivalent to invoking the named tool directly — same args, \
             same response shape.",
            json!({
                "tool": str_field(
                    "Name of the tool to invoke (e.g. `sym`, `refs`, `read`, \
                     `outline`). Memory tools are namespaced as \
                     `memory.<op>` and accepted with or without the prefix."
                ),
                "args": {
                    "type": "object",
                    "description": "Argument object passed verbatim to the \
                                    named tool's input schema.",
                    "additionalProperties": true,
                },
            }),
            &["tool"],
        ),
    ]
}

// Shared MCP-tool schema-builder helpers. `pub(crate)` so `memory.rs`
// can import them and stay schema-shaped consistent with the symbol-
// side tools.

pub(crate) fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing arg: {key}"))
}

pub(crate) fn str_field(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}

pub(crate) fn tool_schema(name: &str, desc: &str, props: Value, required: &[&str]) -> Value {
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
