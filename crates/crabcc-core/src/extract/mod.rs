use crate::resolve::{ImportSpec, Resolver, ScopeCtx, SymbolId};
use crate::store::Store;
use crate::types::{Edge, Symbol, SymbolKind};
use ahash::HashMap;
use anyhow::{anyhow, Result};
use bumpalo::{collections::Vec as BumpVec, Bump};
use std::cell::RefCell;
use std::path::Path;
use tree_sitter::{Node, Parser};

mod data;
pub mod resolve_python;
pub mod resolve_rust;
pub mod resolve_ts;

// Per-thread parser pool. Constructing a `Parser` and calling
// `set_language` is ~5–10 µs of pure overhead per call (LR table init).
// Across a full-repo index that adds up; on the LSP didChange path it
// adds latency the user can feel on a fast typist's keyboard. We keep
// one `Parser` per thread per language and reset between calls.
//
// `thread_local!` keeps this lock-free; the pool never crosses threads.
// The map is keyed on the `&'static str` lang tag we already pass
// everywhere, so no allocation on lookup.
thread_local! {
    static PARSERS: RefCell<HashMap<&'static str, Parser>> = RefCell::new(HashMap::default());
}

fn intern_lang(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "typescript" => "typescript",
        "tsx" => "tsx",
        "javascript" => "javascript",
        "ruby" => "ruby",
        "rust" => "rust",
        "go" => "go",
        "python" => "python",
        "swift" => "swift",
        "bash" => "bash",
        "java" => "java",
        "c" => "c",
        "zig" => "zig",
        // Data formats: markdown/yaml parse through the same pool; their
        // walkers live in `data.rs`. CSV has no grammar and never gets here.
        "markdown" => "markdown",
        "yaml" => "yaml",
        _ => return None,
    })
}

/// Run `f` with a `Parser` already configured for `lang`. The parser is
/// pulled from a per-thread pool and returned afterwards. If no parser
/// exists for this (thread, lang) pair yet, one is created lazily.
fn with_parser<F, T>(lang: &str, f: F) -> Result<T>
where
    F: FnOnce(&mut Parser) -> Result<T>,
{
    let key = intern_lang(lang).ok_or_else(|| anyhow!("unsupported lang: {lang}"))?;
    PARSERS.with(|cell| {
        let mut map = cell.borrow_mut();
        if !map.contains_key(key) {
            let ts_lang = ts_language(key)?;
            let mut p = Parser::new();
            p.set_language(&ts_lang)
                .map_err(|e| anyhow!("set_language({key}): {e}"))?;
            map.insert(key, p);
        }
        let parser = map.get_mut(key).expect("just inserted");
        f(parser)
    })
}

// Per-file bump-arena scratch budget. Tree-sitter's tallest queries on the
// fixtures we care about (mc-mothership, ~1k-line files) build at most a
// few KB of transient strings during impl-retag and signature stitching.
// 4 KB up-front avoids the bump allocator's first-page reallocation for
// any reasonably small file; larger files spill into a second page, which
// is a cheap mmap, not a re-copy.
const SCRATCH_ARENA_BYTES: usize = 4 * 1024;

pub fn detect_lang(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?;
    Some(match ext {
        "ts" => "typescript",
        "tsx" => "tsx",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "rb" | "rake" | "gemspec" => "ruby",
        "rs" => "rust",
        "go" => "go",
        "py" | "pyi" => "python",
        "swift" => "swift",
        "sh" | "bash" | "zsh" => "bash",
        "java" => "java",
        // `.h` headers default to C. C++ isn't supported yet; the C grammar
        // still extracts the C-shaped subset of most headers.
        "c" | "h" => "c",
        "zig" => "zig",
        "md" | "markdown" => "markdown",
        "yaml" | "yml" => "yaml",
        "csv" => "csv",
        _ => return None,
    })
}

pub fn extract_file(file: &str, src: &str, lang: &str) -> Result<Vec<Symbol>> {
    if data::is_data_lang(lang) {
        return data::extract(file, src, lang);
    }
    with_parser(lang, |parser| {
        let tree = parser
            .parse(src, None)
            .ok_or_else(|| anyhow!("parse failed"))?;
        let mut out = Vec::new();
        walk(tree.root_node(), src.as_bytes(), file, lang, None, &mut out);
        Ok(out)
    })
}

/// Extract every call edge in the file. `src_symbol` is the *enclosing*
/// function/method name when the call is inside one (`None` for top-level
/// expression statements). `dst_name` is the bare callee identifier — for
/// member calls like `obj.foo(x)` we record `foo`, matching the unresolved
/// name space the rest of crabcc operates in.
///
/// Co-located with `extract_file` because both share a parser and a walker
/// shape; running them together would double-parse without saving anything.
/// The shared entrypoint is `extract_file_with_edges` below.
pub fn extract_edges(file: &str, src: &str, lang: &str) -> Result<Vec<Edge>> {
    let (_, edges) = extract_file_with_edges(file, src, lang)?;
    Ok(edges)
}

/// Single-parse extraction of both symbols and edges, writing directly to the
/// Store. Pass 1 collects definitions and writes them via `store.insert_symbol`,
/// collecting local defs. Pass 2 resolves use/call sites via the provided
/// `resolver` and writes edges via `store.insert_edge_resolved` or
/// `store.upsert_unresolved_sentinel`.
///
/// The function allocates a per-call `bumpalo::Bump` arena (currently
/// unused by `walk` itself but threaded through so the next phase can
/// switch transient strings to bump-allocated `&str`s. Bump dies with
/// the function, so the entire scratch region frees in one mmap-level
/// op rather than thousands of small `drop(String)` calls.
pub fn extract_file_with_edges_with_resolver(
    file: &str,
    src: &str,
    lang: &str,
    store: &Store,
    resolver: &dyn Resolver,
) -> Result<(Vec<Symbol>, Vec<Edge>)> {
    if data::is_data_lang(lang) {
        let symbols = data::extract(file, src, lang)?;
        persist_data_symbols(store, file, &symbols);
        return Ok((symbols, Vec::new()));
    }
    with_parser(lang, |parser| {
        let tree = parser
            .parse(src, None)
            .ok_or_else(|| anyhow!("parse failed"))?;
        let bytes = src.as_bytes();
        let root = tree.root_node();
        let _scratch = Bump::with_capacity(SCRATCH_ARENA_BYTES);

        // Pass 1: collect definitions, write to store, build local_defs + src_id map
        let mut symbols = Vec::new();
        let mut local_defs: HashMap<String, SymbolId> = HashMap::default();
        // tree-sitter Node::id() returns usize and is stable for the Tree's lifetime.
        let mut node_to_src_id: HashMap<(usize, usize), SymbolId> = HashMap::default();
        walk_with_store(
            root,
            bytes,
            file,
            lang,
            None,
            store,
            &mut symbols,
            &mut local_defs,
            &mut node_to_src_id,
        );

        // Collect imports for ScopeCtx; Vec backing lives in the per-file arena.
        let imports = collect_imports(root, bytes, lang, &_scratch);

        // Pass 2: walk use/call sites, resolve via resolver, write edges
        let mut edges = Vec::new();
        walk_edges_with_resolver(
            root,
            bytes,
            lang,
            None,
            store,
            resolver,
            &local_defs,
            &node_to_src_id,
            &imports,
            file,
            &mut edges,
        );

        Ok((symbols, edges))
    })
}

/// Thin wrapper around `extract_file_with_edges_with_resolver` that uses
/// `NameOnlyResolver` for backward compatibility. Existing callers can
/// continue to use this function with the original signature.
pub fn extract_file_with_edges(
    file: &str,
    src: &str,
    lang: &str,
) -> Result<(Vec<Symbol>, Vec<Edge>)> {
    // Data formats produce symbols only — there is nothing call-shaped in
    // markdown/yaml/csv, so the edge vec is always empty for them.
    if data::is_data_lang(lang) {
        return Ok((data::extract(file, src, lang)?, Vec::new()));
    }
    // Note: This wrapper cannot write to Store (no Store parameter), so we
    // fall back to the original behavior of returning symbols/edges without
    // writing to Store. For Store-backed extraction, call
    // `extract_file_with_edges_with_resolver` directly.
    with_parser(lang, |parser| {
        let tree = parser
            .parse(src, None)
            .ok_or_else(|| anyhow!("parse failed"))?;
        let bytes = src.as_bytes();
        let root = tree.root_node();
        let _scratch = Bump::with_capacity(SCRATCH_ARENA_BYTES);
        let mut symbols = Vec::new();
        walk(root, bytes, file, lang, None, &mut symbols);
        let mut edges = Vec::new();
        walk_edges(root, bytes, lang, None, &mut edges);
        Ok((symbols, edges))
    })
}

/// Public access to the underlying tree-sitter `Language` for a lang
/// tag. Consumers (LSP servers, watchers) that want to keep their own
/// per-document `Parser` + `Tree` and drive incremental reparse can
/// pull the Language here and feed `extract_from_root` for extraction.
pub fn language(lang: &str) -> Result<tree_sitter::Language> {
    ts_language(lang)
}

/// Walk an already-parsed tree to produce symbols + edges, writing to Store.
/// Mirror of `extract_file_with_edges_with_resolver` minus the parse step.
pub fn extract_from_root_with_resolver(
    root: tree_sitter::Node,
    src: &[u8],
    file: &str,
    lang: &str,
    store: &Store,
    resolver: &dyn Resolver,
) -> (Vec<Symbol>, Vec<Edge>) {
    if data::is_data_lang(lang) {
        let mut symbols = Vec::new();
        data::extract_from_node(root, src, file, lang, &mut symbols);
        persist_data_symbols(store, file, &symbols);
        return (symbols, Vec::new());
    }
    let _scratch = Bump::with_capacity(SCRATCH_ARENA_BYTES);

    // Pass1: collect definitions, write to store
    let mut symbols = Vec::new();
    let mut local_defs: HashMap<String, SymbolId> = HashMap::default();
    let mut node_to_src_id: HashMap<(usize, usize), SymbolId> = HashMap::default();
    walk_with_store(
        root,
        src,
        file,
        lang,
        None,
        store,
        &mut symbols,
        &mut local_defs,
        &mut node_to_src_id,
    );

    // Collect imports; Vec backing lives in the per-file arena.
    let imports = collect_imports(root, src, lang, &_scratch);

    // Pass2: resolve use/call sites
    let mut edges = Vec::new();
    walk_edges_with_resolver(
        root,
        src,
        lang,
        None,
        store,
        resolver,
        &local_defs,
        &node_to_src_id,
        &imports,
        file,
        &mut edges,
    );

    (symbols, edges)
}

/// Thin wrapper around `extract_from_root_with_resolver` for backward compatibility.
pub fn extract_from_root(
    root: tree_sitter::Node,
    src: &[u8],
    file: &str,
    lang: &str,
) -> (Vec<Symbol>, Vec<Edge>) {
    if data::is_data_lang(lang) {
        let mut symbols = Vec::new();
        data::extract_from_node(root, src, file, lang, &mut symbols);
        return (symbols, Vec::new());
    }
    // Fall back to original behavior without Store/resolver
    let _scratch = Bump::with_capacity(SCRATCH_ARENA_BYTES);
    let mut symbols = Vec::new();
    walk(root, src, file, lang, None, &mut symbols);
    let mut edges = Vec::new();
    walk_edges(root, src, lang, None, &mut edges);
    (symbols, edges)
}

fn ts_language(lang: &str) -> Result<tree_sitter::Language> {
    // tree-sitter 0.26 + per-language crates ship `LANGUAGE` (or
    // `LANGUAGE_TYPESCRIPT` / `LANGUAGE_TSX` for the polyglot crate) as
    // a `LanguageFn`. `.into()` converts it to `tree_sitter::Language`.
    Ok(match lang {
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "bash" => tree_sitter_bash::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "zig" => tree_sitter_zig::LANGUAGE.into(),
        // Block grammar only — heading extraction doesn't need the inline one.
        "markdown" => tree_sitter_md::LANGUAGE.into(),
        "yaml" => tree_sitter_yaml::LANGUAGE.into(),
        _ => return Err(anyhow!("unsupported lang: {lang}")),
    })
}

fn walk(
    node: Node,
    src: &[u8],
    file: &str,
    lang: &str,
    parent: Option<&str>,
    out: &mut Vec<Symbol>,
) {
    // Rust `impl Foo { ... }` and `impl Trait for Foo { ... }` don't have a
    // `name` field — the parent context for inner methods is the impl-target
    // (the `type` field). We don't emit a symbol for the impl block itself.
    // Top-level fns are `function_item` -> SymbolKind::Function; inside an impl
    // block they should be Method instead. Retag after recursion.
    if lang == "rust" && node.kind() == "impl_item" {
        let impl_target = node
            .child_by_field_name("type")
            .and_then(|n| n.utf8_text(src).ok())
            .map(|s| strip_generics(s).to_string());
        let before = out.len();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk(child, src, file, lang, impl_target.as_deref(), out);
        }
        for sym in out.iter_mut().skip(before) {
            if matches!(sym.kind, SymbolKind::Function)
                && sym.parent.as_deref() == impl_target.as_deref()
            {
                sym.kind = SymbolKind::Method;
            }
        }
        return;
    }

    // Zig declares types as `const Foo = struct { … }` — a variable_declaration
    // wrapping a container node, with the name as a bare identifier child (no
    // `name` field). Emit the container under its const name and descend into
    // the container body so methods get `parent = Foo`. Plain file-scope
    // consts/vars surface as Const/Var; locals inside functions are skipped
    // (they'd flood the index, same rationale as Bash variable_assignment).
    if lang == "zig" && node.kind() == "variable_declaration" {
        if let Some((name, kind, body)) = zig_var_decl(&node, src) {
            let line_start = (node.start_position().row + 1) as u32;
            let line_end = (node.end_position().row + 1) as u32;
            out.push(Symbol {
                name: name.clone(),
                kind,
                signature: signature_for(&node, src, lang),
                parent: parent.map(String::from),
                file: file.to_string(),
                line_start,
                line_end,
                visibility: visibility_for(lang, &node, src),
            });
            if let Some(body) = body {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk(child, src, file, lang, Some(&name), out);
                }
            }
            return;
        }
    }

    if let Some(kind) = symbol_kind_for(lang, node.kind()) {
        if let Some(name) = node_name(&node, src) {
            let n_owned = name.to_string();
            let line_start = (node.start_position().row + 1) as u32;
            let line_end = (node.end_position().row + 1) as u32;
            // Go method_declaration carries its parent type in the `receiver`
            // field, not in lexical scope. Extract it so `parent` reflects the
            // receiver type (with pointer/generic stripped).
            let resolved_parent: Option<String> =
                if lang == "go" && node.kind() == "method_declaration" {
                    go_receiver_type(&node, src)
                } else {
                    parent.map(String::from)
                };
            out.push(Symbol {
                name: n_owned.clone(),
                kind,
                signature: signature_for(&node, src, lang),
                parent: resolved_parent,
                file: file.to_string(),
                line_start,
                line_end,
                visibility: visibility_for(lang, &node, src),
            });
            // Descend with this symbol as the new parent. Leaf symbols have no
            // children, so skip the per-node TreeCursor allocation for them.
            if node.child_count() == 0 {
                return;
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk(child, src, file, lang, Some(&n_owned), out);
            }
            return;
        }
    }
    // Leaf nodes (identifiers, punctuation, keywords) dominate the AST and have
    // no children; skip the per-node TreeCursor allocation for them.
    if node.child_count() == 0 {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, file, lang, parent, out);
    }
}

/// Collect imports from the file for ScopeCtx. Returns empty vec for languages
/// without straightforward import syntax.
fn collect_imports<'b>(
    root: Node,
    src: &[u8],
    lang: &str,
    arena: &'b Bump,
) -> BumpVec<'b, ImportSpec> {
    let mut imports = BumpVec::new_in(arena);
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        collect_imports_from_node(child, src, lang, &mut imports);
    }
    imports
}

fn collect_imports_from_node<'b>(
    node: Node,
    src: &[u8],
    lang: &str,
    out: &mut BumpVec<'b, ImportSpec>,
) {
    match (lang, node.kind()) {
        ("typescript" | "tsx" | "javascript", "import_statement") => {
            // Simplified: collect module name from import statement
            if let Some(source) = node.child_by_field_name("source") {
                if let Ok(module) = source.utf8_text(src) {
                    let module = module.trim_matches('"').trim_matches('\'').to_string();
                    out.push(ImportSpec {
                        local: module.clone(),
                        qualified: module,
                        /* symbols list — not yet broken out per-spec */ // simplified for now
                    });
                }
            }
        }
        ("python", "import_statement" | "import_from_statement") => {
            // Simplified Python import collection
            if let Ok(text) = node.utf8_text(src) {
                out.push(ImportSpec {
                    local: text.to_string(),
                    qualified: text.to_string(),
                    /* symbols list — not yet broken out per-spec */
                });
            }
        }
        ("java", "import_declaration") => {
            if let Ok(text) = node.utf8_text(src) {
                let txt = text
                    .replace("import ", "")
                    .replace(';', "")
                    .trim()
                    .to_string();
                out.push(ImportSpec {
                    local: txt.clone(),
                    qualified: txt,
                    /* symbols list — not yet broken out per-spec */
                });
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_imports_from_node(child, src, lang, out);
    }
}

/// Pass 1 walk: collect definitions, write to Store, populate local_defs and
/// node_to_src_id.
#[allow(clippy::too_many_arguments)]
fn walk_with_store(
    node: Node,
    src: &[u8],
    file: &str,
    lang: &str,
    parent_id: Option<SymbolId>,
    store: &Store,
    out: &mut Vec<Symbol>,
    local_defs: &mut HashMap<String, SymbolId>,
    node_to_src_id: &mut HashMap<(usize, usize), SymbolId>,
) {
    // Handle Rust impl blocks similarly to `walk`
    if lang == "rust" && node.kind() == "impl_item" {
        let impl_target = node
            .child_by_field_name("type")
            .and_then(|n| n.utf8_text(src).ok())
            .map(|s| strip_generics(s).to_string());
        let before = out.len();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk_with_store(
                child,
                src,
                file,
                lang,
                parent_id,
                store,
                out,
                local_defs,
                node_to_src_id,
            );
        }
        // Retag methods inside impl blocks
        for sym in out.iter_mut().skip(before) {
            if matches!(sym.kind, SymbolKind::Function)
                && sym.parent.as_deref() == impl_target.as_deref()
            {
                sym.kind = SymbolKind::Method;
            }
        }
        return;
    }

    // Mirror of walk()'s Zig container handling — see the comment there.
    if lang == "zig" && node.kind() == "variable_declaration" {
        if let Some((name, kind, body)) = zig_var_decl(&node, src) {
            let line_start = (node.start_position().row + 1) as u32;
            let line_end = (node.end_position().row + 1) as u32;
            let signature = signature_for(&node, src, lang);
            let visibility = visibility_for(lang, &node, src);
            let file_id = store.get_file_id(file).ok().flatten().unwrap_or_default();
            let rowid = store
                .insert_symbol(
                    file_id,
                    &name,
                    None,
                    kind,
                    parent_id.map(|s| s.0),
                    line_start as i64,
                    line_end as i64,
                    signature.as_deref(),
                    visibility.as_deref(),
                )
                .unwrap_or(-1);
            if rowid >= 0 {
                let sym_id = SymbolId(rowid);
                local_defs.insert(name.clone(), sym_id);
                node_to_src_id.insert((node.start_byte(), node.end_byte()), sym_id);
                out.push(Symbol {
                    name: name.clone(),
                    kind,
                    signature,
                    parent: None,
                    file: file.to_string(),
                    line_start,
                    line_end,
                    visibility: visibility_for(lang, &node, src),
                });
                if let Some(body) = body {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        walk_with_store(
                            child,
                            src,
                            file,
                            lang,
                            Some(sym_id),
                            store,
                            out,
                            local_defs,
                            node_to_src_id,
                        );
                    }
                }
            }
            return;
        }
    }

    if let Some(kind) = symbol_kind_for(lang, node.kind()) {
        if let Some(name) = node_name(&node, src) {
            let n_owned = name.to_string();
            let line_start = (node.start_position().row + 1) as u32;
            let line_end = (node.end_position().row + 1) as u32;
            let signature = signature_for(&node, src, lang);
            let visibility = visibility_for(lang, &node, src);

            // Get file_id from store (simplified: assume store has this method)
            let file_id = store.get_file_id(file).ok().flatten().unwrap_or_default(); // Fallback to 0 if not found; adjust as needed

            // Write to store
            let rowid = store
                .insert_symbol(
                    file_id,
                    &n_owned,
                    None, // qualified: pass None for now
                    kind,
                    parent_id.map(|s| s.0),
                    line_start as i64,
                    line_end as i64,
                    signature.as_deref(),
                    visibility.as_deref(),
                )
                .unwrap_or(-1);

            if rowid >= 0 {
                let sym_id = SymbolId(rowid);
                // Insert into local_defs (last write wins for duplicates)
                local_defs.insert(n_owned.clone(), sym_id);
                // Map node byte range to src_id
                node_to_src_id.insert((node.start_byte(), node.end_byte()), sym_id);
                // Add to symbols output
                out.push(Symbol {
                    name: n_owned.clone(),
                    kind,
                    signature,
                    parent: None, // parent is tracked via parent_id
                    file: file.to_string(),
                    line_start,
                    line_end,
                    visibility: visibility_for(lang, &node, src),
                });
                // Descend with this symbol as parent
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_with_store(
                        child,
                        src,
                        file,
                        lang,
                        Some(sym_id),
                        store,
                        out,
                        local_defs,
                        node_to_src_id,
                    );
                }
                return;
            }
        }
    }
    // Recurse for non-definition nodes
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_with_store(
            child,
            src,
            file,
            lang,
            parent_id,
            store,
            out,
            local_defs,
            node_to_src_id,
        );
    }
}

/// Pass 2 walk: resolve use/call sites via resolver and write edges.
#[allow(clippy::too_many_arguments)]
fn walk_edges_with_resolver(
    node: Node,
    src: &[u8],
    lang: &str,
    enclosing_id: Option<SymbolId>,
    store: &Store,
    resolver: &dyn Resolver,
    local_defs: &HashMap<String, SymbolId>,
    node_to_src_id: &HashMap<(usize, usize), SymbolId>,
    imports: &[ImportSpec],
    file: &str,
    out: &mut Vec<Edge>,
) {
    // Track enclosing definition's SymbolId
    let new_enclosing_id = if is_callable(lang, node.kind()) {
        // Look up the node's SymbolId from node_to_src_id
        node_to_src_id
            .get(&(node.start_byte(), node.end_byte()))
            .copied()
    } else {
        None
    };
    let next_enclosing = new_enclosing_id.or(enclosing_id);

    // Process call targets
    if let Some((dst_name, line)) = call_target(&node, src, lang) {
        let src_id = next_enclosing;
        if let Some(src_symbol_id) = src_id {
            // Build ScopeCtx
            let scope = ScopeCtx {
                file_id: store.get_file_id(file).ok().flatten().unwrap_or_default(),
                current_module: None, // Simplified; derive from AST if possible
                imports,
                local_defs,
            };
            // Resolve call target
            let dst_id = resolver.resolve_call(&scope, &dst_name);
            let dst_id = match dst_id {
                Some(id) => id,
                None => {
                    // Fallback to unresolved sentinel
                    SymbolId(store.upsert_unresolved_sentinel(&dst_name).unwrap_or(-1))
                }
            };
            // Write edge
            let _ = store.insert_edge_resolved(src_symbol_id.0, dst_id.0, "call", line as i64);
            // Also add to output edges for backward compatibility
            out.push(Edge {
                src_file: String::new(),
                src_symbol: None, // Not needed for store-backed edges
                dst_name,
                kind: "call".into(),
                line,
            });
        }
    }

    // Process ref targets
    if let Some((dst_name, line)) = ref_target(&node, src, lang) {
        let src_id = next_enclosing;
        if let Some(src_symbol_id) = src_id {
            let scope = ScopeCtx {
                file_id: store.get_file_id(file).ok().flatten().unwrap_or_default(),
                current_module: None,
                imports,
                local_defs,
            };
            let dst_id = resolver.resolve_ref(&scope, &dst_name);
            let dst_id = match dst_id {
                Some(id) => id,
                None => SymbolId(store.upsert_unresolved_sentinel(&dst_name).unwrap_or(-1)),
            };
            let _ = store.insert_edge_resolved(src_symbol_id.0, dst_id.0, "ref", line as i64);
            out.push(Edge {
                src_file: String::new(),
                src_symbol: None,
                dst_name,
                kind: "ref".into(),
                line,
            });
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_edges_with_resolver(
            child,
            src,
            lang,
            next_enclosing,
            store,
            resolver,
            local_defs,
            node_to_src_id,
            imports,
            file,
            out,
        );
    }
}

/// Write data-format symbols (markdown/yaml/csv) to the store on the
/// resolver-backed entry points, mirroring what `walk_with_store` does for
/// code. Textual parents (a YAML key's enclosing key) have no row id, so
/// `parent_id` stays NULL — the `parent` string on the wire type carries it.
fn persist_data_symbols(store: &Store, file: &str, symbols: &[Symbol]) {
    let file_id = store.get_file_id(file).ok().flatten().unwrap_or_default();
    for s in symbols {
        let _ = store.insert_symbol(
            file_id,
            &s.name,
            None,
            s.kind,
            None,
            s.line_start as i64,
            s.line_end as i64,
            s.signature.as_deref(),
            s.visibility.as_deref(),
        );
    }
}

/// `Foo<T>` -> `Foo`. The impl-target's tree-sitter node text includes generic
/// params; we strip them so `parent` is the bare type name an agent can grep for.
fn strip_generics(s: &str) -> &str {
    match s.find('<') {
        Some(i) => s[..i].trim(),
        None => s.trim(),
    }
}

/// Extract the receiver type from a Go `method_declaration` node, stripping
/// pointer (`*Repo` -> `Repo`) and any generic params (`Repo[T]` -> `Repo`).
fn go_receiver_type(node: &Node, src: &[u8]) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    for child in receiver.children(&mut cursor) {
        if child.kind() == "parameter_declaration" {
            if let Some(ty) = child.child_by_field_name("type") {
                let raw = ty.utf8_text(src).ok()?;
                let no_ptr = raw.trim_start_matches('*').trim();
                let no_generics = match no_ptr.find('[') {
                    Some(i) => no_ptr[..i].trim(),
                    None => no_ptr,
                };
                return Some(no_generics.to_string());
            }
        }
    }
    None
}

fn node_name<'a>(node: &Node, src: &'a [u8]) -> Option<&'a str> {
    // Swift's `init` / `deinit` decls have no `name` field — the keyword
    // IS the identifier as far as callHierarchy and outline are
    // concerned. Synthesize a static string so the rest of the extractor
    // stays generic.
    match node.kind() {
        "init_declaration" => return Some("init"),
        "deinit_declaration" => return Some("deinit"),
        // C struct/enum/union specifiers appear at use sites too
        // (`struct Bare b;`) — those carry a `name` but no `body`. Only the
        // definition (body present) becomes a symbol; the use-site is noise.
        "struct_specifier" | "enum_specifier" | "union_specifier" => {
            node.child_by_field_name("body")?;
        }
        _ => {}
    }
    if let Some(name) = node.child_by_field_name("name") {
        return name.utf8_text(src).ok();
    }
    // C definitions hide the name inside a declarator chain instead of a
    // `name` field: function_definition -> (pointer_declarator ->)
    // function_declarator -> identifier, and type_definition (`typedef`)
    // -> (pointer_declarator ->) type_identifier. Python/Bash
    // function_definition has a `name` field, so they never reach here.
    match node.kind() {
        "function_definition" | "type_definition" => c_declarator_name(node, src),
        _ => None,
    }
}

/// Unwrap a C declarator chain to the declared identifier. Each wrapper
/// (`pointer_declarator`, `function_declarator`, `array_declarator`, …)
/// nests the next layer in its own `declarator` field.
fn c_declarator_name<'a>(node: &Node, src: &'a [u8]) -> Option<&'a str> {
    let mut decl = node.child_by_field_name("declarator")?;
    loop {
        match decl.kind() {
            "identifier" | "type_identifier" => return decl.utf8_text(src).ok(),
            _ => decl = decl.child_by_field_name("declarator")?,
        }
    }
}

/// Zig `variable_declaration` decoder. Returns `(name, kind, container_body)`:
/// `const Foo = struct {…}` -> (Foo, Struct, Some(struct node)); a plain
/// file-scope `const MAX = 100;` / `var counter = 0;` -> Const/Var with no
/// body. Locals inside functions return None — they'd flood the index.
fn zig_var_decl<'t>(node: &Node<'t>, src: &[u8]) -> Option<(String, SymbolKind, Option<Node<'t>>)> {
    let mut name: Option<String> = None;
    let mut container: Option<(SymbolKind, Node<'t>)> = None;
    let mut is_const = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" if name.is_none() => {
                name = child.utf8_text(src).ok().map(String::from);
            }
            "const" => is_const = true,
            "struct_declaration" => container = Some((SymbolKind::Struct, child)),
            "enum_declaration" => container = Some((SymbolKind::Enum, child)),
            // Zig unions are struct-shaped value types; collapse for v1.
            "union_declaration" => container = Some((SymbolKind::Struct, child)),
            "opaque_declaration" => container = Some((SymbolKind::Type, child)),
            _ => {}
        }
    }
    let name = name.filter(|n| n != "_")?;
    match container {
        Some((kind, body)) => Some((name, kind, Some(body))),
        None => {
            if node.parent().map(|p| p.kind()) == Some("source_file") {
                let kind = if is_const {
                    SymbolKind::Const
                } else {
                    SymbolKind::Var
                };
                Some((name, kind, None))
            } else {
                None
            }
        }
    }
}

/// Walk emitting one edge per call expression. Tracks the immediate enclosing
/// function/method as `src_symbol`; when the call sits at file scope (an
/// `import` side-effect, a top-level smoke test, etc.) we leave it null.
fn walk_edges(node: Node, src: &[u8], lang: &str, enclosing: Option<&str>, out: &mut Vec<Edge>) {
    // If we're entering a callable, push a new enclosing for its body.
    let new_enclosing: Option<String> = if is_callable(lang, node.kind()) {
        node_name(&node, src).map(String::from)
    } else {
        None
    };
    let next = new_enclosing.as_deref().or(enclosing);

    if let Some((dst, line)) = call_target(&node, src, lang) {
        out.push(Edge {
            src_file: String::new(), // store layer keys edges by file_id, not path
            src_symbol: next.map(String::from),
            dst_name: dst,
            kind: "call".into(),
            line,
        });
    }

    if let Some((dst, line)) = ref_target(&node, src, lang) {
        out.push(Edge {
            src_file: String::new(),
            src_symbol: next.map(String::from),
            dst_name: dst,
            kind: "ref".into(),
            line,
        });
    }

    // Leaf nodes have no children; skip the per-node TreeCursor allocation.
    if node.child_count() == 0 {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_edges(child, src, lang, next, out);
    }
}

fn is_callable(lang: &str, kind: &str) -> bool {
    match lang {
        "typescript" | "tsx" => matches!(
            kind,
            "function_declaration"
                | "method_definition"
                | "method_signature"
                | "abstract_method_signature"
                | "function_expression"
                | "arrow_function"
                | "generator_function"
                | "generator_function_declaration"
        ),
        "javascript" => matches!(
            kind,
            "function_declaration"
                | "method_definition"
                | "function_expression"
                | "arrow_function"
                | "generator_function"
                | "generator_function_declaration"
        ),
        "ruby" => matches!(kind, "method" | "singleton_method"),
        "rust" => matches!(kind, "function_item"),
        "go" => matches!(kind, "function_declaration" | "method_declaration"),
        "python" => matches!(kind, "function_definition"),
        "swift" => matches!(
            kind,
            "function_declaration" | "init_declaration" | "deinit_declaration"
        ),
        "bash" => matches!(kind, "function_definition"),
        // Java has no top-level functions — `method_declaration` always
        // sits inside a class/interface/enum body, and `constructor_declaration`
        // is the canonical class constructor.
        "java" => matches!(kind, "method_declaration" | "constructor_declaration"),
        "c" => matches!(kind, "function_definition"),
        "zig" => matches!(kind, "function_declaration"),
        _ => false,
    }
}

/// Returns `(dst_name, 1-based-line)` if this node is a call expression we
/// can extract a callee name from. Falls back to `None` for syntax we
/// can't usefully resolve (e.g. `(a || b)()`, `arr[0]()`).
fn call_target(node: &Node, src: &[u8], lang: &str) -> Option<(String, u32)> {
    let line = (node.start_position().row + 1) as u32;
    match (lang, node.kind()) {
        // TS / TSX / JS share the call_expression node shape.
        ("typescript" | "tsx" | "javascript", "call_expression") => {
            let func = node.child_by_field_name("function")?;
            let dst = match func.kind() {
                "identifier" | "property_identifier" => func.utf8_text(src).ok()?.to_string(),
                "member_expression" => func
                    .child_by_field_name("property")
                    .and_then(|p| p.utf8_text(src).ok())?
                    .to_string(),
                // `import("…")` and other dynamic forms — skip; nothing to
                // attribute to a symbol name.
                _ => return None,
            };
            Some((dst, line))
        }
        // Tree-sitter ruby uses `call` for both `obj.foo(x)` and `foo(x)`.
        ("ruby", "call") => {
            let m = node.child_by_field_name("method")?;
            // The method field can be `identifier` / `constant` / `operator`.
            // Skip operators (`.+`, `.<<`) — they're not interesting graph edges.
            if matches!(m.kind(), "identifier" | "constant") {
                Some((m.utf8_text(src).ok()?.to_string(), line))
            } else {
                None
            }
        }
        // Rust: call_expression has `function` field; macros are macro_invocation.
        // Both unwrap through field/scope expressions to the trailing identifier.
        ("rust", "call_expression") => {
            let func = node.child_by_field_name("function")?;
            rust_callee(&func, src).map(|n| (n, line))
        }
        ("rust", "macro_invocation") => {
            let m = node.child_by_field_name("macro")?;
            match m.kind() {
                "identifier" => Some((m.utf8_text(src).ok()?.to_string(), line)),
                "scoped_identifier" => m
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(src).ok())
                    .map(|s| (s.to_string(), line)),
                _ => None,
            }
        }
        // Go: call_expression with `function` field. Receiver-form `r.Save()`
        // is `selector_expression` whose `field` is the called name.
        ("go", "call_expression") => {
            let func = node.child_by_field_name("function")?;
            match func.kind() {
                "identifier" => Some((func.utf8_text(src).ok()?.to_string(), line)),
                "selector_expression" => func
                    .child_by_field_name("field")
                    .and_then(|f| f.utf8_text(src).ok())
                    .map(|s| (s.to_string(), line)),
                _ => None,
            }
        }
        // Python: `call` has `function` field; attribute access for `obj.foo()`.
        ("python", "call") => {
            let func = node.child_by_field_name("function")?;
            match func.kind() {
                "identifier" => Some((func.utf8_text(src).ok()?.to_string(), line)),
                "attribute" => func
                    .child_by_field_name("attribute")
                    .and_then(|a| a.utf8_text(src).ok())
                    .map(|s| (s.to_string(), line)),
                _ => None,
            }
        }
        // Swift: `call_expression` has no `function` field. The first
        // non-trivia child is the callee — `simple_identifier` for free
        // fns, `navigation_expression` for `obj.foo()`. For the latter,
        // dig into the trailing `navigation_suffix` for the method name.
        ("swift", "call_expression") => {
            let mut cursor = node.walk();
            let target = node.children(&mut cursor).next()?;
            match target.kind() {
                "simple_identifier" => Some((target.utf8_text(src).ok()?.to_string(), line)),
                "navigation_expression" => {
                    let mut sub = target.walk();
                    let mut method: Option<String> = None;
                    for child in target.children(&mut sub) {
                        if child.kind() == "navigation_suffix" {
                            let mut k = child.walk();
                            for grand in child.children(&mut k) {
                                if grand.kind() == "simple_identifier" {
                                    method = grand.utf8_text(src).ok().map(String::from);
                                }
                            }
                        }
                    }
                    Some((method?, line))
                }
                _ => None,
            }
        }
        // Bash: `cmd arg arg` — the `name` field carries the command name.
        // We treat every command invocation as an edge so callHierarchy
        // works for shell-script callgraphs.
        ("bash", "command") => {
            let name = node.child_by_field_name("name")?;
            Some((name.utf8_text(src).ok()?.to_string(), line))
        }
        // Java: `method_invocation` carries the called method's name on its
        // `name` field. We intentionally ignore the receiver (the `object`
        // field) so `obj.foo()` and `Cls.foo()` both surface as `foo`,
        // matching how every other language extractor in this file resolves
        // selector-style calls. `object_creation_expression` (`new Foo(...)`)
        // resolves to the type name so constructor edges land on the class.
        ("java", "method_invocation") => {
            let name = node.child_by_field_name("name")?;
            Some((name.utf8_text(src).ok()?.to_string(), line))
        }
        // C: `function` field is an identifier for `foo(x)`, or a
        // field_expression for function-pointer members (`ops->run(x)`,
        // `obj.cb()`) — take the trailing field name, same unresolved name
        // space as every other selector-style call here.
        ("c", "call_expression") => {
            let func = node.child_by_field_name("function")?;
            match func.kind() {
                "identifier" => Some((func.utf8_text(src).ok()?.to_string(), line)),
                "field_expression" => func
                    .child_by_field_name("field")
                    .and_then(|f| f.utf8_text(src).ok())
                    .map(|s| (s.to_string(), line)),
                _ => None,
            }
        }
        // Zig: `foo(x)` has an identifier `function`; `std.debug.print(…)`
        // is a field_expression whose `member` is the called name. Builtin
        // calls (`@import`) are a separate builtin_function node — skipped
        // on purpose; they're compiler intrinsics, not graph edges.
        ("zig", "call_expression") => {
            let func = node.child_by_field_name("function")?;
            match func.kind() {
                "identifier" => Some((func.utf8_text(src).ok()?.to_string(), line)),
                "field_expression" => func
                    .child_by_field_name("member")
                    .and_then(|m| m.utf8_text(src).ok())
                    .map(|s| (s.to_string(), line)),
                _ => None,
            }
        }
        ("java", "object_creation_expression") => {
            let ty = node.child_by_field_name("type")?;
            // The type field is a type_identifier or generic_type. For the
            // generic case we want just the head (`List<Foo>` -> `List`).
            let raw = match ty.kind() {
                "type_identifier" => ty.utf8_text(src).ok()?.to_string(),
                "generic_type" => ty
                    .child_by_field_name("name")
                    .or_else(|| ty.child(0))
                    .and_then(|n| n.utf8_text(src).ok())
                    .map(|s| strip_generics(s).to_string())?,
                _ => return None,
            };
            Some((raw, line))
        }
        _ => None,
    }
}

/// Rust callees can be `identifier` (`foo()`), `field_expression` (`x.foo()`),
/// `scoped_identifier` (`mod::foo()`), or `generic_function` wrapping any of
/// the above. We unwrap to the trailing simple name — same shape as everywhere
/// else in crabcc.
fn rust_callee(func: &Node, src: &[u8]) -> Option<String> {
    match func.kind() {
        "identifier" => func.utf8_text(src).ok().map(String::from),
        "field_expression" => func
            .child_by_field_name("field")
            .and_then(|f| f.utf8_text(src).ok())
            .map(String::from),
        "scoped_identifier" => func
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .map(String::from),
        "generic_function" => func
            .child_by_field_name("function")
            .and_then(|f| rust_callee(&f, src)),
        _ => None,
    }
}

/// Returns `(dst_name, 1-based-line)` for nodes that reference a type (or
/// other named symbol) outside of definition context — what `lookup refs`
/// surfaces beyond what `call_target` already catches. Currently Rust-only:
/// every `type_identifier` use that isn't the `name` field of a definition
/// (struct / enum / union / type alias / trait / associated_type /
/// type_parameter). Lossy on generic parameters — bare `T` uses emit refs
/// to the parameter name; accepted as noise until lexical scoping lands.
fn ref_target(node: &Node, src: &[u8], lang: &str) -> Option<(String, u32)> {
    if lang != "rust" || node.kind() != "type_identifier" {
        return None;
    }
    if let Some(parent) = node.parent() {
        let parent_defines_name = matches!(
            parent.kind(),
            "struct_item"
                | "enum_item"
                | "union_item"
                | "type_item"
                | "trait_item"
                | "associated_type"
                | "type_parameter"
        );
        if parent_defines_name
            && parent.child_by_field_name("name").map(|n| n.byte_range()) == Some(node.byte_range())
        {
            return None;
        }
    }
    let name = node.utf8_text(src).ok()?.to_string();
    let line = (node.start_position().row + 1) as u32;
    Some((name, line))
}

fn symbol_kind_for(lang: &str, kind: &str) -> Option<SymbolKind> {
    match (lang, kind) {
        ("typescript" | "tsx", k) => match k {
            "function_declaration" => Some(SymbolKind::Function),
            "class_declaration" => Some(SymbolKind::Class),
            "interface_declaration" => Some(SymbolKind::Interface),
            "type_alias_declaration" => Some(SymbolKind::Type),
            "enum_declaration" => Some(SymbolKind::Enum),
            "method_definition" | "method_signature" | "abstract_method_signature" => {
                Some(SymbolKind::Method)
            }
            _ => None,
        },
        ("javascript", k) => match k {
            "function_declaration" => Some(SymbolKind::Function),
            "class_declaration" => Some(SymbolKind::Class),
            "method_definition" => Some(SymbolKind::Method),
            _ => None,
        },
        ("ruby", k) => match k {
            "method" => Some(SymbolKind::Method),
            "singleton_method" => Some(SymbolKind::Method),
            "class" => Some(SymbolKind::Class),
            "module" => Some(SymbolKind::Class), // collapse module into class for v1
            _ => None,
        },
        ("rust", k) => match k {
            // function_item is top-level fn; inside impl_item it's a method —
            // the kind is fixed at extract time, but `parent` carries the impl
            // context so callers can distinguish.
            // function_signature_item covers trait-body declarations like
            // `fn hello(&self);` — same shape, no body.
            "function_item" | "function_signature_item" => Some(SymbolKind::Function),
            "struct_item" => Some(SymbolKind::Struct),
            "enum_item" => Some(SymbolKind::Enum),
            "trait_item" => Some(SymbolKind::Trait),
            "mod_item" => Some(SymbolKind::Class), // collapse mod into class for v1
            "const_item" => Some(SymbolKind::Const),
            "static_item" => Some(SymbolKind::Var),
            "type_item" => Some(SymbolKind::Type),
            "macro_definition" => Some(SymbolKind::Macro),
            _ => None,
        },
        ("go", k) => match k {
            "function_declaration" => Some(SymbolKind::Function),
            "method_declaration" => Some(SymbolKind::Method),
            // Go wraps the named declaration in `*_spec` nodes inside the
            // `*_declaration`. The spec carries the `name` field; the outer
            // declaration does not.
            "type_spec" => Some(SymbolKind::Type),
            "const_spec" => Some(SymbolKind::Const),
            "var_spec" => Some(SymbolKind::Var),
            _ => None,
        },
        ("python", k) => match k {
            "function_definition" => Some(SymbolKind::Function),
            "class_definition" => Some(SymbolKind::Class),
            // decorated_definition wraps a function/class — descend without
            // emitting; the inner definition carries the actual name.
            _ => None,
        },
        ("swift", k) => match k {
            "function_declaration" => Some(SymbolKind::Function),
            "init_declaration" | "deinit_declaration" => Some(SymbolKind::Method),
            "class_declaration" => Some(SymbolKind::Class),
            "protocol_declaration" => Some(SymbolKind::Interface),
            "enum_declaration" => Some(SymbolKind::Enum),
            "typealias_declaration" => Some(SymbolKind::Type),
            "property_declaration" => Some(SymbolKind::Var),
            _ => None,
        },
        // Bash: only `function_definition` becomes a symbol.
        // variable_assignment is intentionally not surfaced — inside fn bodies
        // it floods the outline; we'd want a parent-aware emission that the
        // generic walk() doesn't model. Leave it.
        ("bash", "function_definition") => Some(SymbolKind::Function),
        ("java", k) => match k {
            "class_declaration" => Some(SymbolKind::Class),
            "interface_declaration" => Some(SymbolKind::Interface),
            // Annotation types (`@interface Foo`) are interfaces in Java's
            // type system; collapse them under Interface for v1.
            "annotation_type_declaration" => Some(SymbolKind::Interface),
            "enum_declaration" => Some(SymbolKind::Enum),
            // Java records (Java 14+) are concise immutable data classes —
            // closer in spirit to Rust structs than to Java classes.
            "record_declaration" => Some(SymbolKind::Struct),
            // Java has no top-level functions: every method lives in a
            // class/interface/enum body. The walk() recursion sets parent
            // correctly via the enclosing declaration.
            "method_declaration" | "constructor_declaration" => Some(SymbolKind::Method),
            _ => None,
        },
        ("c", k) => match k {
            "function_definition" => Some(SymbolKind::Function),
            // Specifiers double as use sites (`struct Foo x;`) — node_name
            // gates on the `body` field so only definitions emit.
            "struct_specifier" => Some(SymbolKind::Struct),
            "union_specifier" => Some(SymbolKind::Struct),
            "enum_specifier" => Some(SymbolKind::Enum),
            "type_definition" => Some(SymbolKind::Type),
            "preproc_def" | "preproc_function_def" => Some(SymbolKind::Macro),
            _ => None,
        },
        // Zig: only fns route through the generic table. Containers
        // (`const Foo = struct {…}`) are handled by the variable_declaration
        // special case in walk()/walk_with_store — they have no `name` field.
        ("zig", "function_declaration") => Some(SymbolKind::Function),
        _ => None,
    }
}

fn signature_for(node: &Node, src: &[u8], lang: &str) -> Option<String> {
    let body = node
        .child_by_field_name("body")
        .or_else(|| node.child_by_field_name("value"));
    let start = node.start_byte();
    // Tree-sitter byte offsets are normally in-bounds for `src`, but a tree
    // reused as an incremental-parse hint whose `InputEdit`s don't match the
    // text it's reparsed against (e.g. concurrent edits to the same document
    // upstream of the LSP indexer) can hand back stale ranges with end < start
    // or past the buffer. Slice through `get()` so a bad range degrades to
    // "no signature" instead of panicking — a panic here would poison the
    // caller's store mutex and wedge all further indexing.
    let tail = src.get(start..)?;
    let end = body.map(|b| b.start_byte()).unwrap_or_else(|| {
        // No body — take just the first line. Reuse the bounds-checked
        // `tail` (start <= len already guaranteed by the `?` above) instead
        // of re-slicing `src[start..]`, which would also leave `tail` unused.
        let nl = tail.iter().position(|&b| b == b'\n').unwrap_or_default();
        start + nl
    });
    let raw = std::str::from_utf8(src.get(start..end)?).ok()?;
    Some(compact(raw, lang))
}

fn compact(s: &str, lang: &str) -> String {
    // Strip trailing Ruby line-comments BEFORE collapsing whitespace, so
    // we drop the comment cleanly even if it spans multiple physical lines.
    let cleaned = if lang == "ruby" {
        s.lines()
            .map(|line| match line.find(" # ") {
                Some(i) => &line[..i],
                None => line.trim_end_matches('#'),
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        s.to_string()
    };
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    joined
        .trim_end_matches('{')
        .trim_end_matches('=')
        .trim()
        .to_string()
}

fn visibility_for(lang: &str, node: &Node, src: &[u8]) -> Option<String> {
    match lang {
        "typescript" | "tsx" => {
            // Tree-sitter wraps exported decls in `export_statement`.
            let mut p = node.parent();
            while let Some(n) = p {
                if n.kind() == "export_statement" {
                    return Some("pub".into());
                }
                p = n.parent();
            }
            None
        }
        "ruby" => {
            // Visibility in Ruby is positional via `private`/`public` calls — skip for v1.
            let _ = (node, src);
            None
        }
        "rust" => {
            // visibility_modifier child carries the literal "pub", "pub(crate)",
            // "pub(super)", or "pub(self)". Absence means private (None).
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "visibility_modifier" {
                    if let Ok(text) = child.utf8_text(src) {
                        return Some(text.split_whitespace().collect::<Vec<_>>().join(""));
                    }
                }
            }
            None
        }
        "go" => {
            // Go exports by capitalization. No AST node — read the name field.
            let _ = src;
            let name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok())?;
            let first = name.chars().next()?;
            if first.is_ascii_uppercase() {
                Some("pub".into())
            } else {
                Some("priv".into())
            }
        }
        "python" => {
            // Convention: `_foo` is private, `__foo` is name-mangled private,
            // `__foo__` is a dunder and remains public by Python's rules.
            let _ = src;
            let name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok())?;
            let is_dunder = name.starts_with("__") && name.ends_with("__") && name.len() >= 4;
            if is_dunder {
                Some("pub".into())
            } else if name.starts_with('_') {
                Some("priv".into())
            } else {
                Some("pub".into())
            }
        }
        "swift" => {
            // Walk the `modifiers` child for one of the access tokens.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "modifiers" {
                    let text = child.utf8_text(src).unwrap_or_default();
                    for token in ["public", "open", "internal", "fileprivate", "private"] {
                        if text.contains(token) {
                            return Some(token.to_string());
                        }
                    }
                }
            }
            None
        }
        "bash" => {
            // No visibility concept in shell; functions are global within
            // the process. Leave as None.
            let _ = (node, src);
            None
        }
        "java" => {
            // Java modifiers are a child node (kind = "modifiers") containing
            // tokens like `public`, `protected`, `private`, plus annotations
            // and `static`/`final`/`abstract`. Absent modifier == package-
            // private (the default). We surface "pub"/"protected"/"priv"/"pkg"
            // in priority order so an agent can tell apart hidden vs default.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "modifiers" {
                    let text = child.utf8_text(src).unwrap_or_default();
                    // Private wins over protected wins over public if multiple
                    // appeared (which would be invalid Java, but be defensive).
                    if text.contains("private") {
                        return Some("priv".into());
                    }
                    if text.contains("protected") {
                        return Some("protected".into());
                    }
                    if text.contains("public") {
                        return Some("pub".into());
                    }
                    // Modifiers node present but no access keyword → still pkg.
                    return Some("pkg".into());
                }
            }
            // No modifiers child at all → package-private (the default).
            Some("pkg".into())
        }
        "c" => {
            // `static` at file scope means internal linkage — the closest C
            // gets to private. Everything else (extern or unmarked) is
            // linkable from other translation units; leave None rather than
            // claiming "pub" for declarations we can't see the linkage of.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "storage_class_specifier"
                    && child.utf8_text(src).ok() == Some("static")
                {
                    return Some("priv".into());
                }
            }
            None
        }
        "zig" => {
            // `pub` is a bare keyword child on fn/const declarations.
            // Absence means module-private by Zig's rules.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "pub" {
                    return Some("pub".into());
                }
            }
            let _ = src;
            Some("priv".into())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn names(syms: &[Symbol]) -> Vec<&str> {
        syms.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn detect_lang_extensions() {
        assert_eq!(detect_lang(&PathBuf::from("a.ts")), Some("typescript"));
        assert_eq!(detect_lang(&PathBuf::from("a.tsx")), Some("tsx"));
        assert_eq!(detect_lang(&PathBuf::from("a.js")), Some("javascript"));
        assert_eq!(detect_lang(&PathBuf::from("a.mjs")), Some("javascript"));
        assert_eq!(detect_lang(&PathBuf::from("a.rb")), Some("ruby"));
        assert_eq!(detect_lang(&PathBuf::from("a.rs")), Some("rust"));
        assert_eq!(detect_lang(&PathBuf::from("a.go")), Some("go"));
        assert_eq!(detect_lang(&PathBuf::from("a.py")), Some("python"));
        assert_eq!(detect_lang(&PathBuf::from("a.pyi")), Some("python"));
        assert_eq!(detect_lang(&PathBuf::from("a.java")), Some("java"));
        assert_eq!(detect_lang(&PathBuf::from("a.c")), Some("c"));
        assert_eq!(detect_lang(&PathBuf::from("a.h")), Some("c"));
        assert_eq!(detect_lang(&PathBuf::from("a.zig")), Some("zig"));
        assert_eq!(detect_lang(&PathBuf::from("a.md")), Some("markdown"));
        assert_eq!(detect_lang(&PathBuf::from("a.markdown")), Some("markdown"));
        assert_eq!(detect_lang(&PathBuf::from("a.yml")), Some("yaml"));
        assert_eq!(detect_lang(&PathBuf::from("a.yaml")), Some("yaml"));
        assert_eq!(detect_lang(&PathBuf::from("a.csv")), Some("csv"));
        assert_eq!(detect_lang(&PathBuf::from("Rakefile")), None);
        assert_eq!(detect_lang(&PathBuf::from("a.txt")), None);
    }

    // ---- TypeScript / JavaScript symbols ----

    #[test]
    fn ts_js_symbols() {
        // function export: name, kind, pub visibility, line, signature.
        {
            let src = "export function foo(a: string): number { return 0; }";
            let syms = extract_file("a.ts", src, "typescript").unwrap();
            assert_eq!(syms.len(), 1, "got: {syms:?}");
            let s = &syms[0];
            assert_eq!(s.name, "foo");
            assert!(matches!(s.kind, SymbolKind::Function));
            assert_eq!(s.visibility.as_deref(), Some("pub"));
            assert_eq!(s.line_start, 1);
            let sig = s.signature.as_deref().unwrap_or_default();
            assert!(
                sig.contains("foo"),
                "signature should contain name: {sig:?}"
            );
        }
        // class method gets class parent + Method kind.
        {
            let src = "class Greeter {\n  greet(name: string): string { return name; }\n}\n";
            let syms = extract_file("a.ts", src, "typescript").unwrap();
            let n = names(&syms);
            assert!(n.contains(&"Greeter"), "names: {n:?}");
            assert!(n.contains(&"greet"), "names: {n:?}");
            let m = syms.iter().find(|s| s.name == "greet").unwrap();
            assert_eq!(m.parent.as_deref(), Some("Greeter"));
            assert!(matches!(m.kind, SymbolKind::Method));
        }
        // interface + type alias.
        {
            let src = "interface User { id: number; }\ntype Id = string;\n";
            let syms = extract_file("a.ts", src, "typescript").unwrap();
            let n = names(&syms);
            assert!(n.contains(&"User"));
            assert!(n.contains(&"Id"));
            let i = syms.iter().find(|s| s.name == "User").unwrap();
            assert!(matches!(i.kind, SymbolKind::Interface));
        }
        // JS function declaration.
        {
            let src = "function add(a, b) { return a + b; }";
            let syms = extract_file("a.js", src, "javascript").unwrap();
            assert_eq!(syms.len(), 1);
            assert_eq!(syms[0].name, "add");
            assert!(matches!(syms[0].kind, SymbolKind::Function));
        }
    }

    // ---- Ruby symbols ----

    #[test]
    fn ruby_symbols() {
        // class method gets class parent + Method kind.
        {
            let src = "class Foo\n  def bar(x)\n    x\n  end\nend\n";
            let syms = extract_file("a.rb", src, "ruby").unwrap();
            let n = names(&syms);
            assert!(n.contains(&"Foo"));
            assert!(n.contains(&"bar"));
            let m = syms.iter().find(|s| s.name == "bar").unwrap();
            assert_eq!(m.parent.as_deref(), Some("Foo"));
            assert!(matches!(m.kind, SymbolKind::Method));
        }
        // module + module function.
        {
            let src = "module Auth\n  def self.sign_in(u); end\nend\n";
            let syms = extract_file("a.rb", src, "ruby").unwrap();
            let n = names(&syms);
            assert!(n.contains(&"Auth"));
            assert!(n.contains(&"sign_in"));
        }
        // signature strips trailing `#` comments.
        {
            let src = "class User # the number seems arbitrary, ported from legacy\n  # extra notes\n  def name; end\nend\n";
            let syms = extract_file("a.rb", src, "ruby").unwrap();
            let cls = syms.iter().find(|s| s.name == "User").unwrap();
            let sig = cls.signature.as_deref().unwrap_or_default();
            assert!(
                !sig.contains('#'),
                "signature should not leak '#' comments, got: {sig:?}"
            );
            assert!(sig.starts_with("class User"), "got: {sig:?}");
        }
    }

    // ---- Rust ----

    #[test]
    fn rust_pub_function_with_visibility() {
        let src = "pub fn add(a: i32, b: i32) -> i32 { a + b }\n";
        let syms = extract_file("a.rs", src, "rust").unwrap();
        assert_eq!(syms.len(), 1, "got: {syms:?}");
        let s = &syms[0];
        assert_eq!(s.name, "add");
        assert!(matches!(s.kind, SymbolKind::Function));
        assert_eq!(s.visibility.as_deref(), Some("pub"));
        let sig = s.signature.as_deref().unwrap_or_default();
        assert!(sig.contains("fn add"), "got: {sig:?}");
    }

    #[test]
    fn rust_private_function_has_no_visibility() {
        let src = "fn helper() -> u8 { 0 }\n";
        let syms = extract_file("a.rs", src, "rust").unwrap();
        assert_eq!(syms.len(), 1);
        assert!(syms[0].visibility.is_none());
    }

    #[test]
    fn rust_pub_crate_visibility_preserved() {
        let src = "pub(crate) fn internal() {}\npub(super) fn parent_only() {}\n";
        let syms = extract_file("a.rs", src, "rust").unwrap();
        let internal = syms.iter().find(|s| s.name == "internal").unwrap();
        let parent_only = syms.iter().find(|s| s.name == "parent_only").unwrap();
        assert_eq!(internal.visibility.as_deref(), Some("pub(crate)"));
        assert_eq!(parent_only.visibility.as_deref(), Some("pub(super)"));
    }

    #[test]
    fn rust_struct_with_inherent_impl_method_has_parent() {
        let src = "pub struct User { id: u64 }\nimpl User { pub fn new(id: u64) -> Self { Self { id } } }\n";
        let syms = extract_file("a.rs", src, "rust").unwrap();
        let n = names(&syms);
        assert!(n.contains(&"User"), "names: {n:?}");
        assert!(n.contains(&"new"), "names: {n:?}");
        let new = syms.iter().find(|s| s.name == "new").unwrap();
        assert_eq!(new.parent.as_deref(), Some("User"));
        // function_item inside an impl_item gets retagged to Method.
        assert!(matches!(new.kind, SymbolKind::Method), "{new:?}");
    }

    #[test]
    fn rust_impl_trait_for_method_has_concrete_type_parent() {
        // For `impl Display for User { fn fmt(...) {} }` the method's parent
        // should be the concrete type `User`, not the trait.
        let src = "struct User;\nimpl std::fmt::Display for User { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) } }\n";
        let syms = extract_file("a.rs", src, "rust").unwrap();
        let m = syms.iter().find(|s| s.name == "fmt").unwrap();
        assert_eq!(m.parent.as_deref(), Some("User"));
        assert!(matches!(m.kind, SymbolKind::Method));
    }

    #[test]
    fn rust_generic_impl_target_strips_params() {
        // `impl<T> Container<T> { fn get(&self) {} }` → parent = "Container".
        let src = "struct Container<T>(T);\nimpl<T> Container<T> { pub fn get(&self) -> &T { &self.0 } }\n";
        let syms = extract_file("a.rs", src, "rust").unwrap();
        let get = syms.iter().find(|s| s.name == "get").unwrap();
        assert_eq!(get.parent.as_deref(), Some("Container"));
    }

    #[test]
    fn rust_trait_enum_const_static_type() {
        let src = "pub trait Greeter { fn hello(&self); }\n\
                   pub enum Mode { Hits, Files, Count }\n\
                   pub const MAX: u32 = 100;\n\
                   pub static NAME: &str = \"crabcc\";\n\
                   pub type Id = u64;\n";
        let syms = extract_file("a.rs", src, "rust").unwrap();
        let by = |needle: &str| {
            syms.iter()
                .find(|s| s.name == needle)
                .unwrap_or_else(|| panic!("missing {needle}: {:?}", names(&syms)))
                .clone()
        };
        assert!(matches!(by("Greeter").kind, SymbolKind::Trait));
        assert!(matches!(by("Mode").kind, SymbolKind::Enum));
        assert!(matches!(by("MAX").kind, SymbolKind::Const));
        assert!(matches!(by("NAME").kind, SymbolKind::Var)); // static_item -> Var
        assert!(matches!(by("Id").kind, SymbolKind::Type));
    }

    #[test]
    fn rust_macro_rules_emits_macro_kind() {
        let src = "macro_rules! say { ($n:expr) => { println!(\"hi {}\", $n) }; }\n";
        let syms = extract_file("a.rs", src, "rust").unwrap();
        let m = syms.iter().find(|s| s.name == "say").unwrap();
        assert!(matches!(m.kind, SymbolKind::Macro), "{m:?}");
    }

    #[test]
    fn rust_mod_collapses_to_class_kind() {
        // mod_item has a `name` field; we collapse mod into Class for v1
        // (same as Ruby module). Inner symbols carry `parent=<mod_name>`.
        let src = "pub mod inner { pub fn q() {} }\n";
        let syms = extract_file("a.rs", src, "rust").unwrap();
        let m = syms.iter().find(|s| s.name == "inner").unwrap();
        assert!(matches!(m.kind, SymbolKind::Class));
        let q = syms.iter().find(|s| s.name == "q").unwrap();
        assert_eq!(q.parent.as_deref(), Some("inner"));
    }

    #[test]
    fn rust_strip_generics_helper() {
        assert_eq!(strip_generics("Foo"), "Foo");
        assert_eq!(strip_generics("Foo<T>"), "Foo");
        assert_eq!(strip_generics("Container<T, U>"), "Container");
        assert_eq!(strip_generics("  Spaced  "), "Spaced");
    }

    #[test]
    fn rust_struct_usage_emits_ref_edges() {
        let src = "pub struct Store;\nimpl Store {}\nfn run() -> Store { panic!() }\n";
        let (_, edges) = extract_file_with_edges("a.rs", src, "rust").unwrap();
        let refs: Vec<_> = edges
            .iter()
            .filter(|e| e.kind == "ref" && e.dst_name == "Store")
            .collect();
        // `impl Store` (line 2) + return type `-> Store` (line 3).
        assert!(
            refs.len() >= 2,
            "expected ≥2 ref edges for Store, got: {refs:?}"
        );
    }

    #[test]
    fn rust_struct_definition_does_not_self_ref() {
        let src = "pub struct Store;\n";
        let (_, edges) = extract_file_with_edges("a.rs", src, "rust").unwrap();
        let refs: Vec<_> = edges
            .iter()
            .filter(|e| e.kind == "ref" && e.dst_name == "Store")
            .collect();
        assert!(
            refs.is_empty(),
            "definition name must not emit a self-ref, got: {refs:?}"
        );
    }

    #[test]
    fn rust_generic_type_param_definition_does_not_self_ref() {
        // `<T>` declares T; the inner type_identifier on its `name` field
        // is the declaration site, not a use.
        let src = "fn id<T>(x: T) -> T { x }\n";
        let (_, edges) = extract_file_with_edges("a.rs", src, "rust").unwrap();
        let t_decls: Vec<_> = edges
            .iter()
            .filter(|e| e.kind == "ref" && e.dst_name == "T" && e.line == 1)
            .collect();
        // We accept use-site emissions for `x: T` and `-> T` — confirm at
        // least one such use exists AND that the count matches the two use
        // sites rather than including the `<T>` declaration.
        assert!(
            t_decls.len() == 2,
            "expected exactly 2 T uses (param type + return type), got: {t_decls:?}"
        );
    }

    // ---- Go symbols ----

    #[test]
    fn go_symbols() {
        // visibility from capitalization (exported vs unexported).
        {
            let src = "package x\nfunc Add(a, b int) int { return a + b }\nfunc helper() {}\n";
            let syms = extract_file("a.go", src, "go").unwrap();
            let add = syms.iter().find(|s| s.name == "Add").unwrap();
            assert!(matches!(add.kind, SymbolKind::Function));
            assert_eq!(add.visibility.as_deref(), Some("pub"));
            let helper = syms.iter().find(|s| s.name == "helper").unwrap();
            assert_eq!(helper.visibility.as_deref(), Some("priv"));
        }
        // pointer receiver strips to the type name.
        {
            let src = "package x\ntype Repo struct{}\nfunc (r *Repo) Save() error { return nil }\n";
            let syms = extract_file("a.go", src, "go").unwrap();
            let save = syms.iter().find(|s| s.name == "Save").unwrap();
            assert!(matches!(save.kind, SymbolKind::Method));
            assert_eq!(save.parent.as_deref(), Some("Repo"));
        }
        // value receiver.
        {
            let src =
                "package x\ntype User struct{}\nfunc (u User) Name() string { return \"\" }\n";
            let syms = extract_file("a.go", src, "go").unwrap();
            let name = syms.iter().find(|s| s.name == "Name").unwrap();
            assert_eq!(name.parent.as_deref(), Some("User"));
            assert!(matches!(name.kind, SymbolKind::Method));
        }
        // type / const / var declarations.
        {
            let src = "package x\ntype ID int\nconst Max = 100\nvar Default = \"hi\"\n";
            let syms = extract_file("a.go", src, "go").unwrap();
            let by = |needle: &str| {
                syms.iter()
                    .find(|s| s.name == needle)
                    .unwrap_or_else(|| panic!("missing {needle}: {:?}", names(&syms)))
                    .clone()
            };
            assert!(matches!(by("ID").kind, SymbolKind::Type));
            assert!(matches!(by("Max").kind, SymbolKind::Const));
            assert!(matches!(by("Default").kind, SymbolKind::Var));
        }
        // pointer + generic receiver strips to the type name.
        {
            let src = "package x\ntype Box[T any] struct{}\nfunc (b *Box[T]) Open() {}\n";
            let syms = extract_file("a.go", src, "go").unwrap();
            let open = syms.iter().find(|s| s.name == "Open").unwrap();
            assert_eq!(open.parent.as_deref(), Some("Box"));
        }
    }

    // ---- Python symbols ----

    #[test]
    fn python_symbols() {
        // visibility from leading underscores; dunder is public.
        {
            let src = "def add(a, b):\n    return a + b\n\ndef _internal():\n    pass\n\ndef __mangled():\n    pass\n";
            let syms = extract_file("a.py", src, "python").unwrap();
            let add = syms.iter().find(|s| s.name == "add").unwrap();
            let internal = syms.iter().find(|s| s.name == "_internal").unwrap();
            let mangled = syms.iter().find(|s| s.name == "__mangled").unwrap();
            assert_eq!(add.visibility.as_deref(), Some("pub"));
            assert_eq!(internal.visibility.as_deref(), Some("priv"));
            assert_eq!(mangled.visibility.as_deref(), Some("priv"));
            assert!(matches!(add.kind, SymbolKind::Function));
        }
        {
            // Dunder methods are public by Python's rules despite `__`.
            let src = "class A:\n    def __init__(self):\n        pass\n";
            let syms = extract_file("a.py", src, "python").unwrap();
            let init = syms.iter().find(|s| s.name == "__init__").unwrap();
            assert_eq!(init.visibility.as_deref(), Some("pub"));
        }
        // async def -> Function kind.
        {
            let src = "async def fetch(url):\n    return url\n";
            let syms = extract_file("a.py", src, "python").unwrap();
            let fetch = syms.iter().find(|s| s.name == "fetch").unwrap();
            assert!(matches!(fetch.kind, SymbolKind::Function));
        }
        // class method gets class parent.
        {
            let src = "class User:\n    def name(self):\n        return ''\n";
            let syms = extract_file("a.py", src, "python").unwrap();
            let user = syms.iter().find(|s| s.name == "User").unwrap();
            assert!(matches!(user.kind, SymbolKind::Class));
            let name = syms.iter().find(|s| s.name == "name").unwrap();
            assert_eq!(name.parent.as_deref(), Some("User"));
            assert!(matches!(name.kind, SymbolKind::Function));
        }
        // @dataclass-decorated class unwraps to the inner class.
        {
            let src = "from dataclasses import dataclass\n\n@dataclass\nclass Point:\n    x: int\n    y: int\n";
            let syms = extract_file("a.py", src, "python").unwrap();
            let point = syms.iter().find(|s| s.name == "Point").unwrap();
            assert!(matches!(point.kind, SymbolKind::Class));
        }
        // decorated async def.
        {
            let src = "@retry\nasync def fetch_user(uid):\n    return uid\n";
            let syms = extract_file("a.py", src, "python").unwrap();
            let fetch = syms.iter().find(|s| s.name == "fetch_user").unwrap();
            assert!(matches!(fetch.kind, SymbolKind::Function));
        }
    }

    // ---- Cross-cutting extractor edge cases ----

    #[test]
    fn rust_impl_with_multiple_methods_all_get_method_kind() {
        // Stress the impl_item retag path: every fn under the impl must come
        // out as Method, not Function, even when there are several.
        let src =
            "struct Repo;\nimpl Repo {\n  pub fn one() {}\n  pub fn two() {}\n  fn three() {}\n}\n";
        let syms = extract_file("a.rs", src, "rust").unwrap();
        for n in ["one", "two", "three"] {
            let s = syms.iter().find(|s| s.name == n).unwrap();
            assert!(matches!(s.kind, SymbolKind::Method), "{n} -> {s:?}");
            assert_eq!(s.parent.as_deref(), Some("Repo"));
        }
    }

    #[test]
    fn rust_top_level_fn_outside_impl_stays_function() {
        // Regression guard for the impl_item retag — top-level fns must NOT
        // get retagged, even when they appear in the same file as an impl.
        let src = "pub fn standalone() {}\nstruct Repo;\nimpl Repo { fn member() {} }\n";
        let syms = extract_file("a.rs", src, "rust").unwrap();
        let standalone = syms.iter().find(|s| s.name == "standalone").unwrap();
        assert!(matches!(standalone.kind, SymbolKind::Function));
        assert!(standalone.parent.is_none());
        let member = syms.iter().find(|s| s.name == "member").unwrap();
        assert!(matches!(member.kind, SymbolKind::Method));
    }

    #[test]
    fn rust_nested_mod_propagates_innermost_parent() {
        let src = "pub mod outer {\n  pub mod inner {\n    pub fn deep() {}\n  }\n}\n";
        let syms = extract_file("a.rs", src, "rust").unwrap();
        let deep = syms.iter().find(|s| s.name == "deep").unwrap();
        assert_eq!(deep.parent.as_deref(), Some("inner"));
    }

    #[test]
    fn rust_trait_methods_have_trait_as_parent() {
        // Methods declared inside `trait Greeter { fn hello(); }` should
        // attribute their parent to the trait — same shape as Class methods.
        let src =
            "pub trait Greeter {\n  fn hello(&self);\n  fn goodbye(&self) { /* default */ }\n}\n";
        let syms = extract_file("a.rs", src, "rust").unwrap();
        let hello = syms.iter().find(|s| s.name == "hello").unwrap();
        let goodbye = syms.iter().find(|s| s.name == "goodbye").unwrap();
        assert_eq!(hello.parent.as_deref(), Some("Greeter"));
        assert_eq!(goodbye.parent.as_deref(), Some("Greeter"));
    }

    #[test]
    fn python_nested_class_and_method_chain() {
        // Tests the parent walk through class_definition children. The inner
        // class should have parent=Outer; its methods parent=Inner.
        let src = "class Outer:\n    class Inner:\n        def deep(self):\n            return 1\n";
        let syms = extract_file("a.py", src, "python").unwrap();
        let inner = syms.iter().find(|s| s.name == "Inner").unwrap();
        assert_eq!(inner.parent.as_deref(), Some("Outer"));
        let deep = syms.iter().find(|s| s.name == "deep").unwrap();
        assert_eq!(deep.parent.as_deref(), Some("Inner"));
    }

    #[test]
    fn python_signature_does_not_leak_pound_comments() {
        // Compaction strips `# ...` for Ruby; for Python the pound is also a
        // comment marker. We don't apply Ruby's stripping logic to Python (the
        // syntax differs), but signatures must not contain spurious pound chars
        // mid-line that would confuse downstream parsers — verify the captured
        // signature stays sensible for a typical decorated def.
        let src = "def add(a, b):\n    \"\"\"docstring\"\"\"\n    return a + b\n";
        let syms = extract_file("a.py", src, "python").unwrap();
        let s = syms.iter().find(|s| s.name == "add").unwrap();
        let sig = s.signature.as_deref().unwrap_or_default();
        assert!(sig.contains("def add"), "got: {sig:?}");
    }

    #[test]
    fn go_function_inside_method_block_does_not_collide() {
        // Local closure / func literal inside a method body should not pollute
        // the symbol table — only the outer method should be emitted at the
        // top level. Tree-sitter-go does not expose anonymous func literals
        // as named declarations, so this is a sanity check.
        let src = "package x\ntype Repo struct{}\nfunc (r *Repo) Save() {\n  helper := func() int { return 1 }\n  _ = helper\n}\n";
        let syms = extract_file("a.go", src, "go").unwrap();
        let save = syms.iter().find(|s| s.name == "Save").unwrap();
        assert_eq!(save.parent.as_deref(), Some("Repo"));
        // No phantom `helper` symbol from the local `:=` assignment.
        assert!(syms.iter().all(|s| s.name != "helper"));
    }

    #[test]
    fn cross_lang_dispatch_preserves_per_lang_kinds() {
        // Same source byte string parsed under different langs must NOT bleed
        // kinds across — a regression guard for the (lang, node_kind) match.
        let rust_src = "pub fn x() {}";
        let go_src = "package x\nfunc X() {}";
        let py_src = "def x():\n    pass\n";
        let r = extract_file("a.rs", rust_src, "rust").unwrap();
        let g = extract_file("a.go", go_src, "go").unwrap();
        let p = extract_file("a.py", py_src, "python").unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(g.len(), 1);
        assert_eq!(p.len(), 1);
        assert_eq!(r[0].name, "x");
        assert_eq!(g[0].name, "X");
        assert_eq!(p[0].name, "x");
    }

    #[test]
    fn empty_source_yields_no_symbols() {
        // Defensive: empty / whitespace-only files must not panic.
        for lang in ["rust", "go", "python", "typescript", "ruby"] {
            let ext = match lang {
                "rust" => "rs",
                "go" => "go",
                "python" => "py",
                "typescript" => "ts",
                "ruby" => "rb",
                _ => "txt",
            };
            let file = format!("empty.{ext}");
            let s = extract_file(&file, "", lang).unwrap();
            assert!(
                s.is_empty(),
                "expected no symbols for empty {lang}, got: {s:?}"
            );
            let s2 = extract_file(&file, "\n\n   \n", lang).unwrap();
            assert!(s2.is_empty(), "expected no symbols for whitespace {lang}");
        }
    }

    #[test]
    fn malformed_source_does_not_panic() {
        // Tree-sitter is permissive; even broken syntax should produce SOME
        // tree (possibly with ERROR nodes), not a panic. We don't assert on
        // the exact symbol set — just that extraction returns.
        let _ = extract_file("a.rs", "fn broken( {", "rust").unwrap();
        let _ = extract_file("a.go", "package x\nfunc broken( {", "go").unwrap();
        let _ = extract_file("a.py", "def broken(:\n", "python").unwrap();
    }

    #[test]
    fn unsupported_lang_errors() {
        assert!(extract_file("a.txt", "hello", "klingon").is_err());
    }

    // ---- Edge extraction ----

    fn edges(file: &str, src: &str, lang: &str) -> Vec<Edge> {
        extract_edges(file, src, lang).unwrap()
    }

    fn dst_names(es: &[Edge]) -> Vec<&str> {
        es.iter().map(|e| e.dst_name.as_str()).collect()
    }

    #[test]
    fn ts_js_ruby_edges() {
        // TS: bare calls attribute to the enclosing caller.
        {
            let src = "function high(){ low(); mid(); }\nfunction low(){}\nfunction mid(){}\n";
            let es = edges("a.ts", src, "typescript");
            let from_high: Vec<&str> = es
                .iter()
                .filter(|e| e.src_symbol.as_deref() == Some("high"))
                .map(|e| e.dst_name.as_str())
                .collect();
            assert!(from_high.contains(&"low"), "edges: {es:?}");
            assert!(from_high.contains(&"mid"), "edges: {es:?}");
        }
        // TS: member call keeps the property name, not the receiver.
        {
            let src = "function f(){ obj.greet('hi'); }\n";
            let es = edges("a.ts", src, "typescript");
            let dst: Vec<&str> = es.iter().map(|e| e.dst_name.as_str()).collect();
            assert!(dst.contains(&"greet"), "edges: {es:?}");
            assert!(!dst.contains(&"obj"), "edges: {es:?}");
        }
        // TS: top-level call has no caller.
        {
            let src = "function greet(n){ return n; }\ngreet('world');\n";
            let es = edges("a.ts", src, "typescript");
            let top = es
                .iter()
                .find(|e| e.dst_name == "greet" && e.src_symbol.is_none());
            assert!(top.is_some(), "expected top-level greet call: {es:?}");
        }
        // TS: anonymous arrow body call still emits exactly one edge.
        {
            let src = "const f = () => { foo(); };\n";
            let es = edges("a.ts", src, "typescript");
            let foo_calls: Vec<&Edge> = es.iter().filter(|e| e.dst_name == "foo").collect();
            assert_eq!(foo_calls.len(), 1, "edges: {es:?}");
        }
        // TS: method-body call attributes to the method, not the class.
        {
            let src =
                "class G { greet(n){ return helper(n); } }\nfunction helper(x){ return x; }\n";
            let es = edges("a.ts", src, "typescript");
            let helper_call = es.iter().find(|e| e.dst_name == "helper").unwrap();
            assert_eq!(helper_call.src_symbol.as_deref(), Some("greet"));
        }
        // JS: basic call edge.
        {
            let src = "function a(){ b(); }\nfunction b(){}\n";
            let es = edges("a.js", src, "javascript");
            assert!(dst_names(&es).contains(&"b"));
        }
        // Ruby: only parenthesized calls are edges (bare ident is not).
        {
            let src = "def high\n  low\n  mid()\nend\ndef low; end\ndef mid; end\n";
            let es = edges("a.rb", src, "ruby");
            let from_high: Vec<&str> = es
                .iter()
                .filter(|e| e.src_symbol.as_deref() == Some("high"))
                .map(|e| e.dst_name.as_str())
                .collect();
            assert!(from_high.contains(&"mid"), "edges: {es:?}");
        }
        // Ruby: chained receiver call (Foo.new.bar) emits bar + new, attributes to `go`.
        {
            let src = "class C\n  def go\n    Foo.new.bar(1)\n  end\nend\n";
            let es = edges("a.rb", src, "ruby");
            let n = dst_names(&es);
            assert!(n.contains(&"bar"), "edges: {es:?}");
            assert!(n.contains(&"new"), "edges: {es:?}");
            let bar = es.iter().find(|e| e.dst_name == "bar").unwrap();
            assert_eq!(bar.src_symbol.as_deref(), Some("go"));
        }
        // Single parse returns both symbols and edges.
        {
            let src = "function f(){ g(); }\nfunction g(){}\n";
            let (syms, es) = extract_file_with_edges("a.ts", src, "typescript").unwrap();
            assert!(syms.iter().any(|s| s.name == "f"));
            assert!(syms.iter().any(|s| s.name == "g"));
            assert!(es.iter().any(|e| e.dst_name == "g"));
        }
        // Unsupported language errors.
        assert!(extract_edges("a.txt", "x", "klingon").is_err());
    }

    // ---- Java symbols + edges ----

    #[test]
    fn java_symbols_and_edges() {
        // class + method parent/kind.
        {
            let src =
                "public class Greeter {\n  public String greet(String name) { return name; }\n}\n";
            let syms = extract_file("Greeter.java", src, "java").unwrap();
            let n = names(&syms);
            assert!(n.contains(&"Greeter"), "names: {n:?}");
            assert!(n.contains(&"greet"), "names: {n:?}");
            let cls = syms.iter().find(|s| s.name == "Greeter").unwrap();
            assert!(matches!(cls.kind, SymbolKind::Class));
            let m = syms.iter().find(|s| s.name == "greet").unwrap();
            assert_eq!(m.parent.as_deref(), Some("Greeter"));
            assert!(matches!(m.kind, SymbolKind::Method));
        }
        // constructor is a Method with the class as parent.
        {
            let src = "public class Foo {\n  public Foo(int x) {}\n}\n";
            let syms = extract_file("Foo.java", src, "java").unwrap();
            let ctor = syms
                .iter()
                .find(|s| s.name == "Foo" && matches!(s.kind, SymbolKind::Method));
            assert!(ctor.is_some(), "expected constructor symbol; got: {syms:?}");
            assert_eq!(ctor.unwrap().parent.as_deref(), Some("Foo"));
        }
        // interface + enum kinds.
        {
            let src = "public interface User { String name(); }\npublic enum Color { RED, GREEN, BLUE }\n";
            let syms = extract_file("a.java", src, "java").unwrap();
            let n = names(&syms);
            assert!(n.contains(&"User"));
            assert!(n.contains(&"Color"));
            assert!(matches!(
                syms.iter().find(|s| s.name == "User").unwrap().kind,
                SymbolKind::Interface
            ));
            assert!(matches!(
                syms.iter().find(|s| s.name == "Color").unwrap().kind,
                SymbolKind::Enum
            ));
        }
        // record -> Struct.
        {
            let src = "public record Point(int x, int y) {}\n";
            let syms = extract_file("Point.java", src, "java").unwrap();
            let p = syms.iter().find(|s| s.name == "Point").unwrap();
            assert!(matches!(p.kind, SymbolKind::Struct), "got: {:?}", p.kind);
        }
        // visibility levels (pub / protected / priv / pkg).
        {
            let src = "\npublic class Outer {\n  public void pubMethod() {}\n  protected void protMethod() {}\n  private void privMethod() {}\n  void pkgMethod() {}\n}\n";
            let syms = extract_file("Outer.java", src, "java").unwrap();
            let v = |name: &str| {
                syms.iter()
                    .find(|s| s.name == name)
                    .and_then(|s| s.visibility.clone())
                    .unwrap_or_default()
            };
            assert_eq!(v("Outer"), "pub");
            assert_eq!(v("pubMethod"), "pub");
            assert_eq!(v("protMethod"), "protected");
            assert_eq!(v("privMethod"), "priv");
            assert_eq!(v("pkgMethod"), "pkg");
        }
        // method-invocation edges attribute to the enclosing method.
        {
            let src = "\nclass C {\n  void high() {\n    helper();\n    other.foo();\n  }\n  void helper() {}\n}\n";
            let es = edges("C.java", src, "java");
            let n = dst_names(&es);
            assert!(n.contains(&"helper"), "edges: {es:?}");
            assert!(n.contains(&"foo"), "edges: {es:?}");
            let helper = es.iter().find(|e| e.dst_name == "helper").unwrap();
            assert_eq!(helper.src_symbol.as_deref(), Some("high"));
        }
        // constructor-call edges resolve to the type (generic head stripped).
        {
            let src =
                "\nclass C {\n  void make() {\n    new Foo();\n    new Bar<String>();\n  }\n}\n";
            let es = edges("C.java", src, "java");
            let n = dst_names(&es);
            assert!(n.contains(&"Foo"), "edges: {es:?}");
            assert!(n.contains(&"Bar"), "edges: {es:?}");
        }
    }

    // ---- C symbols + edges ----

    #[test]
    fn c_symbols_and_edges() {
        // fn name digs through the declarator chain; static -> priv linkage.
        {
            let src = "static int helper(int a) { return a; }\nint *get_ptr(void) { return 0; }\n";
            let syms = extract_file("a.c", src, "c").unwrap();
            let helper = syms.iter().find(|s| s.name == "helper").unwrap();
            assert!(matches!(helper.kind, SymbolKind::Function));
            assert_eq!(helper.visibility.as_deref(), Some("priv"));
            // pointer return type wraps the function_declarator one level deeper.
            let get_ptr = syms.iter().find(|s| s.name == "get_ptr").unwrap();
            assert!(matches!(get_ptr.kind, SymbolKind::Function));
            assert!(get_ptr.visibility.is_none());
        }
        // struct/enum/union definitions emit; bare use sites must not.
        {
            let src = "struct Point { int x; };\nenum Color { RED, GREEN };\nunion U { int i; float f; };\nvoid f(void) { struct Point p; (void)p; }\n";
            let syms = extract_file("a.c", src, "c").unwrap();
            let point: Vec<_> = syms.iter().filter(|s| s.name == "Point").collect();
            assert_eq!(point.len(), 1, "use site must not emit: {syms:?}");
            assert!(matches!(point[0].kind, SymbolKind::Struct));
            assert!(matches!(
                syms.iter().find(|s| s.name == "Color").unwrap().kind,
                SymbolKind::Enum
            ));
            assert!(matches!(
                syms.iter().find(|s| s.name == "U").unwrap().kind,
                SymbolKind::Struct
            ));
        }
        // typedef + #define; `typedef struct Pair {…} Pair;` emits both the
        // struct definition and the typedef alias.
        {
            let src = "#define MAX 100\n#define SQ(x) ((x)*(x))\ntypedef int MyInt;\ntypedef struct Pair { int a; } Pair;\n";
            let syms = extract_file("a.h", src, "c").unwrap();
            assert!(matches!(
                syms.iter().find(|s| s.name == "MAX").unwrap().kind,
                SymbolKind::Macro
            ));
            assert!(matches!(
                syms.iter().find(|s| s.name == "SQ").unwrap().kind,
                SymbolKind::Macro
            ));
            assert!(matches!(
                syms.iter().find(|s| s.name == "MyInt").unwrap().kind,
                SymbolKind::Type
            ));
            let pair_kinds: Vec<_> = syms
                .iter()
                .filter(|s| s.name == "Pair")
                .map(|s| s.kind)
                .collect();
            assert!(pair_kinds.contains(&SymbolKind::Struct), "{syms:?}");
            assert!(pair_kinds.contains(&SymbolKind::Type), "{syms:?}");
        }
        // call edges attribute to the enclosing fn; function-pointer member
        // calls (`ops->run()`) keep the trailing field name.
        {
            let src =
                "void low(void) {}\nvoid high(struct Ops *ops) {\n  low();\n  ops->run(1);\n}\n";
            let es = edges("a.c", src, "c");
            let low = es.iter().find(|e| e.dst_name == "low").unwrap();
            assert_eq!(low.src_symbol.as_deref(), Some("high"));
            assert!(dst_names(&es).contains(&"run"), "edges: {es:?}");
        }
    }

    // ---- Zig symbols + edges ----

    #[test]
    fn zig_symbols_and_edges() {
        let src = "const std = @import(\"std\");\n\
                   pub const MAX: u32 = 100;\n\
                   var counter: u32 = 0;\n\
                   pub const Point = struct {\n    x: i32,\n    pub fn norm(self: Point) i32 { return self.x; }\n};\n\
                   const Color = enum { red, green };\n\
                   fn helper(a: i32) i32 { return a; }\n\
                   pub fn main() void {\n    const local = helper(1);\n    _ = local;\n    std.debug.print(\"hi\", .{});\n}\n";
        let (syms, es) = extract_file_with_edges("a.zig", src, "zig").unwrap();
        let by = |needle: &str| {
            syms.iter()
                .find(|s| s.name == needle)
                .unwrap_or_else(|| panic!("missing {needle}: {:?}", names(&syms)))
                .clone()
        };
        // const-wrapped containers get the container kind, not Const.
        let point = by("Point");
        assert!(matches!(point.kind, SymbolKind::Struct), "{point:?}");
        assert_eq!(point.visibility.as_deref(), Some("pub"));
        assert!(matches!(by("Color").kind, SymbolKind::Enum));
        // method inside the struct carries the container as parent.
        let norm = by("norm");
        assert_eq!(norm.parent.as_deref(), Some("Point"));
        assert!(matches!(norm.kind, SymbolKind::Function));
        // file-scope const/var; `pub` vs unmarked visibility.
        assert!(matches!(by("MAX").kind, SymbolKind::Const));
        assert_eq!(by("MAX").visibility.as_deref(), Some("pub"));
        assert!(matches!(by("counter").kind, SymbolKind::Var));
        assert_eq!(by("counter").visibility.as_deref(), Some("priv"));
        assert!(matches!(by("std").kind, SymbolKind::Const));
        // fns.
        assert!(matches!(by("helper").kind, SymbolKind::Function));
        assert_eq!(by("main").visibility.as_deref(), Some("pub"));
        // locals inside fn bodies do not flood the index.
        assert!(syms.iter().all(|s| s.name != "local"), "{:?}", names(&syms));
        // edges: bare call + field call, both attributed to main.
        let helper_call = es
            .iter()
            .find(|e| e.dst_name == "helper" && e.src_symbol.as_deref() == Some("main"))
            .unwrap_or_else(|| panic!("missing helper edge: {es:?}"));
        assert_eq!(helper_call.kind, "call");
        assert!(
            es.iter()
                .any(|e| e.dst_name == "print" && e.src_symbol.as_deref() == Some("main")),
            "edges: {es:?}"
        );
        // @import is a builtin, not a call edge.
        assert!(es.iter().all(|e| e.dst_name != "import"), "edges: {es:?}");
    }

    // ---- Data formats: Markdown / YAML / CSV ----

    #[test]
    fn markdown_heading_symbols() {
        let src = "# Title\n\nIntro text.\n\n## Section\n\n### Sub\n\nSetext\n======\n";
        let (syms, es) = extract_file_with_edges("README.md", src, "markdown").unwrap();
        assert!(es.is_empty(), "markdown must not emit edges: {es:?}");
        let by = |needle: &str| {
            syms.iter()
                .find(|s| s.name == needle)
                .unwrap_or_else(|| panic!("missing {needle}: {:?}", names(&syms)))
                .clone()
        };
        assert!(matches!(by("Title").kind, SymbolKind::Class));
        assert_eq!(by("Title").line_start, 1);
        assert!(matches!(by("Section").kind, SymbolKind::Type));
        assert!(matches!(by("Sub").kind, SymbolKind::Function));
        // setext H1 (`====` underline) maps like an ATX H1.
        assert!(matches!(by("Setext").kind, SymbolKind::Class));
    }

    #[test]
    fn yaml_keys_two_levels_deep() {
        let src =
            "name: ci\non:\n  push:\n    branches: [main]\njobs:\n  build:\n    runs-on: ubuntu\n";
        let syms = extract_file("ci.yml", src, "yaml").unwrap();
        let n = names(&syms);
        assert!(n.contains(&"name"), "names: {n:?}");
        assert!(n.contains(&"jobs"), "names: {n:?}");
        let build = syms.iter().find(|s| s.name == "build").unwrap();
        assert_eq!(build.parent.as_deref(), Some("jobs"));
        assert!(matches!(build.kind, SymbolKind::Var));
        // depth cap: level-3 keys are nav noise, not symbols.
        assert!(!n.contains(&"runs-on"), "names: {n:?}");
        assert!(!n.contains(&"branches"), "names: {n:?}");
    }

    #[test]
    fn csv_header_columns() {
        // Quoted field keeps its embedded comma; rows below the header are
        // data, not symbols.
        let src = "id,name,\"full, title\",amount\n1,a,b,2\n";
        let syms = extract_file("data.csv", src, "csv").unwrap();
        let n = names(&syms);
        assert_eq!(n, vec!["id", "name", "full, title", "amount"], "{syms:?}");
        assert!(syms.iter().all(|s| matches!(s.kind, SymbolKind::Var)));
        assert!(syms.iter().all(|s| s.line_start == 1));
        // Leading blank lines shift the header line.
        let syms = extract_file("data.csv", "\n\nid,name\n", "csv").unwrap();
        assert_eq!(syms[0].line_start, 3);
        // Header-less (empty) files yield nothing.
        assert!(extract_file("data.csv", "", "csv").unwrap().is_empty());
        assert!(extract_file("data.csv", "\n  \n", "csv")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn new_langs_empty_source_yields_no_symbols() {
        for (lang, ext) in [
            ("c", "c"),
            ("zig", "zig"),
            ("markdown", "md"),
            ("yaml", "yml"),
            ("csv", "csv"),
        ] {
            let file = format!("empty.{ext}");
            let s = extract_file(&file, "", lang).unwrap();
            assert!(s.is_empty(), "expected no symbols for empty {lang}: {s:?}");
        }
    }
}
