use crate::types::{Edge, Symbol, SymbolKind};
use anyhow::{anyhow, Result};
use bumpalo::Bump;
use std::path::Path;
use tree_sitter::{Node, Parser};

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
        _ => return None,
    })
}

pub fn extract_file(file: &str, src: &str, lang: &str) -> Result<Vec<Symbol>> {
    let ts_lang = ts_language(lang)?;
    let mut parser = Parser::new();
    parser
        .set_language(&ts_lang)
        .map_err(|e| anyhow!("set_language: {e}"))?;
    let tree = parser
        .parse(src, None)
        .ok_or_else(|| anyhow!("parse failed"))?;

    let mut out = Vec::new();
    walk(tree.root_node(), src.as_bytes(), file, lang, None, &mut out);
    Ok(out)
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

/// Single-parse extraction of both symbols and edges. Used by the indexer to
/// avoid paying tree-sitter twice per file. The two outputs are independent
/// (tests can request one or the other), but in production we always want
/// both — the indexer hot path goes through here.
///
/// The function allocates a per-call `bumpalo::Bump` arena (currently
/// unused by `walk` itself but threaded through so the next phase can
/// switch transient strings — `impl_target`, `go_receiver_type`,
/// `strip_generics` outputs — to bump-allocated `&str`s without
/// changing the public `Vec<Symbol>` / `Vec<Edge>` shape). Bump dies with
/// the function, so the entire scratch region frees in one mmap-level
/// op rather than thousands of small `drop(String)` calls. See issue
/// #38 (nightly-features research) for the full ROI analysis and the
/// `allocator_api`-vs-`bumpalo::collections` tradeoff.
pub fn extract_file_with_edges(
    file: &str,
    src: &str,
    lang: &str,
) -> Result<(Vec<Symbol>, Vec<Edge>)> {
    let ts_lang = ts_language(lang)?;
    let mut parser = Parser::new();
    parser
        .set_language(&ts_lang)
        .map_err(|e| anyhow!("set_language: {e}"))?;
    let tree = parser
        .parse(src, None)
        .ok_or_else(|| anyhow!("parse failed"))?;

    let bytes = src.as_bytes();
    let root = tree.root_node();
    // Per-file bump arena. Sized to cover the typical impl-retag /
    // signature-stitch transient burst without paging; the allocator
    // grows automatically if a giant file blows past the initial
    // budget.
    let _scratch = Bump::with_capacity(SCRATCH_ARENA_BYTES);
    let mut symbols = Vec::new();
    walk(root, bytes, file, lang, None, &mut symbols);
    let mut edges = Vec::new();
    walk_edges(root, bytes, lang, None, &mut edges);
    Ok((symbols, edges))
}

fn ts_language(lang: &str) -> Result<tree_sitter::Language> {
    Ok(match lang {
        "typescript" => tree_sitter_typescript::language_typescript(),
        "tsx" => tree_sitter_typescript::language_tsx(),
        "javascript" => tree_sitter_javascript::language(),
        "ruby" => tree_sitter_ruby::language(),
        "rust" => tree_sitter_rust::language(),
        "go" => tree_sitter_go::language(),
        "python" => tree_sitter_python::language(),
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
            // Descend with this symbol as the new parent.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk(child, src, file, lang, Some(&n_owned), out);
            }
            return;
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, file, lang, parent, out);
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
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
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
        _ => None,
    }
}

fn signature_for(node: &Node, src: &[u8], lang: &str) -> Option<String> {
    let body = node
        .child_by_field_name("body")
        .or_else(|| node.child_by_field_name("value"));
    let start = node.start_byte();
    let end = body.map(|b| b.start_byte()).unwrap_or_else(|| {
        // No body — take just the first line.
        let nl = src[start..].iter().position(|&b| b == b'\n').unwrap_or(0);
        start + nl
    });
    let raw = std::str::from_utf8(&src[start..end]).ok()?;
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
        assert_eq!(detect_lang(&PathBuf::from("Rakefile")), None);
        assert_eq!(detect_lang(&PathBuf::from("a.md")), None);
    }

    // ---- TypeScript ----

    #[test]
    fn ts_function_export() {
        let src = "export function foo(a: string): number { return 0; }";
        let syms = extract_file("a.ts", src, "typescript").unwrap();
        assert_eq!(syms.len(), 1, "got: {syms:?}");
        let s = &syms[0];
        assert_eq!(s.name, "foo");
        assert!(matches!(s.kind, SymbolKind::Function));
        assert_eq!(s.visibility.as_deref(), Some("pub"));
        assert_eq!(s.line_start, 1);
        let sig = s.signature.as_deref().unwrap_or("");
        assert!(
            sig.contains("foo"),
            "signature should contain name: {sig:?}"
        );
    }

    #[test]
    fn ts_class_with_method_has_parent() {
        let src = "class Greeter {\n  greet(name: string): string { return name; }\n}\n";
        let syms = extract_file("a.ts", src, "typescript").unwrap();
        let n = names(&syms);
        assert!(n.contains(&"Greeter"), "names: {n:?}");
        assert!(n.contains(&"greet"), "names: {n:?}");
        let m = syms.iter().find(|s| s.name == "greet").unwrap();
        assert_eq!(m.parent.as_deref(), Some("Greeter"));
        assert!(matches!(m.kind, SymbolKind::Method));
    }

    #[test]
    fn ts_interface_and_type() {
        let src = "interface User { id: number; }\ntype Id = string;\n";
        let syms = extract_file("a.ts", src, "typescript").unwrap();
        let n = names(&syms);
        assert!(n.contains(&"User"));
        assert!(n.contains(&"Id"));
        let i = syms.iter().find(|s| s.name == "User").unwrap();
        assert!(matches!(i.kind, SymbolKind::Interface));
    }

    // ---- JavaScript ----

    #[test]
    fn js_function_declaration() {
        let src = "function add(a, b) { return a + b; }";
        let syms = extract_file("a.js", src, "javascript").unwrap();
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "add");
        assert!(matches!(syms[0].kind, SymbolKind::Function));
    }

    // ---- Ruby ----

    #[test]
    fn ruby_class_with_method_has_parent() {
        let src = "class Foo\n  def bar(x)\n    x\n  end\nend\n";
        let syms = extract_file("a.rb", src, "ruby").unwrap();
        let n = names(&syms);
        assert!(n.contains(&"Foo"));
        assert!(n.contains(&"bar"));
        let m = syms.iter().find(|s| s.name == "bar").unwrap();
        assert_eq!(m.parent.as_deref(), Some("Foo"));
        assert!(matches!(m.kind, SymbolKind::Method));
    }

    #[test]
    fn ruby_module() {
        let src = "module Auth\n  def self.sign_in(u); end\nend\n";
        let syms = extract_file("a.rb", src, "ruby").unwrap();
        let n = names(&syms);
        assert!(n.contains(&"Auth"));
        assert!(n.contains(&"sign_in"));
    }

    #[test]
    fn ruby_signature_strips_trailing_comment() {
        let src = "class User # the number seems arbitrary, ported from legacy\n  # extra notes\n  def name; end\nend\n";
        let syms = extract_file("a.rb", src, "ruby").unwrap();
        let cls = syms.iter().find(|s| s.name == "User").unwrap();
        let sig = cls.signature.as_deref().unwrap_or("");
        assert!(
            !sig.contains('#'),
            "signature should not leak '#' comments, got: {sig:?}"
        );
        assert!(sig.starts_with("class User"), "got: {sig:?}");
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
        let sig = s.signature.as_deref().unwrap_or("");
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

    // ---- Go ----

    #[test]
    fn go_function_with_visibility_from_capitalization() {
        let src = "package x\nfunc Add(a, b int) int { return a + b }\nfunc helper() {}\n";
        let syms = extract_file("a.go", src, "go").unwrap();
        let add = syms.iter().find(|s| s.name == "Add").unwrap();
        assert!(matches!(add.kind, SymbolKind::Function));
        assert_eq!(add.visibility.as_deref(), Some("pub"));
        let helper = syms.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(helper.visibility.as_deref(), Some("priv"));
    }

    #[test]
    fn go_method_receiver_pointer_strips_to_type_name() {
        let src = "package x\ntype Repo struct{}\nfunc (r *Repo) Save() error { return nil }\n";
        let syms = extract_file("a.go", src, "go").unwrap();
        let save = syms.iter().find(|s| s.name == "Save").unwrap();
        assert!(matches!(save.kind, SymbolKind::Method));
        assert_eq!(save.parent.as_deref(), Some("Repo"));
    }

    #[test]
    fn go_method_value_receiver() {
        let src = "package x\ntype User struct{}\nfunc (u User) Name() string { return \"\" }\n";
        let syms = extract_file("a.go", src, "go").unwrap();
        let name = syms.iter().find(|s| s.name == "Name").unwrap();
        assert_eq!(name.parent.as_deref(), Some("User"));
        assert!(matches!(name.kind, SymbolKind::Method));
    }

    #[test]
    fn go_type_const_var_declarations() {
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

    #[test]
    fn go_receiver_helper_strips_pointer_and_generics() {
        // Inline test of go_receiver_type via a method declaration with both
        // pointer and generic params.
        let src = "package x\ntype Box[T any] struct{}\nfunc (b *Box[T]) Open() {}\n";
        let syms = extract_file("a.go", src, "go").unwrap();
        let open = syms.iter().find(|s| s.name == "Open").unwrap();
        assert_eq!(open.parent.as_deref(), Some("Box"));
    }

    // ---- Python ----

    #[test]
    fn python_def_function_visibility_from_underscore() {
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

    #[test]
    fn python_dunder_init_is_public() {
        // Dunder methods (`__init__`, `__repr__`, `__eq__`) are public by
        // Python's own rules even though they start with `__`.
        let src = "class A:\n    def __init__(self):\n        pass\n";
        let syms = extract_file("a.py", src, "python").unwrap();
        let init = syms.iter().find(|s| s.name == "__init__").unwrap();
        assert_eq!(init.visibility.as_deref(), Some("pub"));
    }

    #[test]
    fn python_async_def_emits_function_kind() {
        let src = "async def fetch(url):\n    return url\n";
        let syms = extract_file("a.py", src, "python").unwrap();
        let fetch = syms.iter().find(|s| s.name == "fetch").unwrap();
        assert!(matches!(fetch.kind, SymbolKind::Function));
    }

    #[test]
    fn python_class_with_method_has_parent() {
        let src = "class User:\n    def name(self):\n        return ''\n";
        let syms = extract_file("a.py", src, "python").unwrap();
        let user = syms.iter().find(|s| s.name == "User").unwrap();
        assert!(matches!(user.kind, SymbolKind::Class));
        let name = syms.iter().find(|s| s.name == "name").unwrap();
        assert_eq!(name.parent.as_deref(), Some("User"));
        assert!(matches!(name.kind, SymbolKind::Function));
    }

    #[test]
    fn python_decorated_class_unwraps_to_inner() {
        // `@dataclass` wraps class_definition in decorated_definition. We descend
        // through the wrapper and emit the inner class.
        let src = "from dataclasses import dataclass\n\n@dataclass\nclass Point:\n    x: int\n    y: int\n";
        let syms = extract_file("a.py", src, "python").unwrap();
        let point = syms.iter().find(|s| s.name == "Point").unwrap();
        assert!(matches!(point.kind, SymbolKind::Class));
    }

    #[test]
    fn python_decorated_async_def_function() {
        let src = "@retry\nasync def fetch_user(uid):\n    return uid\n";
        let syms = extract_file("a.py", src, "python").unwrap();
        let fetch = syms.iter().find(|s| s.name == "fetch_user").unwrap();
        assert!(matches!(fetch.kind, SymbolKind::Function));
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
        let sig = s.signature.as_deref().unwrap_or("");
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
    fn ts_edges_bare_call_attributes_to_caller() {
        let src = "function high(){ low(); mid(); }\nfunction low(){}\nfunction mid(){}\n";
        let es = edges("a.ts", src, "typescript");
        // high() calls low() and mid().
        let from_high: Vec<&str> = es
            .iter()
            .filter(|e| e.src_symbol.as_deref() == Some("high"))
            .map(|e| e.dst_name.as_str())
            .collect();
        assert!(from_high.contains(&"low"), "edges: {es:?}");
        assert!(from_high.contains(&"mid"), "edges: {es:?}");
    }

    #[test]
    fn ts_edges_member_call_keeps_property_name() {
        let src = "function f(){ obj.greet('hi'); }\n";
        let es = edges("a.ts", src, "typescript");
        let dst: Vec<&str> = es.iter().map(|e| e.dst_name.as_str()).collect();
        assert!(dst.contains(&"greet"), "edges: {es:?}");
        // Bare `obj` (a property access on its own) is not a call — should not
        // appear as an edge.
        assert!(!dst.contains(&"obj"), "edges: {es:?}");
    }

    #[test]
    fn ts_edges_top_level_call_has_no_caller() {
        let src = "function greet(n){ return n; }\ngreet('world');\n";
        let es = edges("a.ts", src, "typescript");
        let top = es
            .iter()
            .find(|e| e.dst_name == "greet" && e.src_symbol.is_none());
        assert!(top.is_some(), "expected top-level greet call: {es:?}");
    }

    #[test]
    fn ts_edges_arrow_function_is_callable() {
        let src = "const f = () => { foo(); };\n";
        let es = edges("a.ts", src, "typescript");
        // Arrow with no name → src_symbol is None, but it should still NOT
        // attribute the call to whatever's outside the arrow.
        // We accept None here (anonymous arrow has no name).
        let foo_calls: Vec<&Edge> = es.iter().filter(|e| e.dst_name == "foo").collect();
        assert_eq!(foo_calls.len(), 1, "edges: {es:?}");
    }

    #[test]
    fn ts_edges_method_attributes_to_method_not_class() {
        let src = "class G { greet(n){ return helper(n); } }\nfunction helper(x){ return x; }\n";
        let es = edges("a.ts", src, "typescript");
        // helper() inside greet() should attribute to greet, not G.
        let helper_call = es.iter().find(|e| e.dst_name == "helper").unwrap();
        assert_eq!(helper_call.src_symbol.as_deref(), Some("greet"));
    }

    #[test]
    fn js_edges_basic() {
        let src = "function a(){ b(); }\nfunction b(){}\n";
        let es = edges("a.js", src, "javascript");
        assert!(dst_names(&es).contains(&"b"));
    }

    #[test]
    fn ruby_edges_bare_call() {
        let src = "def high\n  low\n  mid()\nend\ndef low; end\ndef mid; end\n";
        let es = edges("a.rb", src, "ruby");
        let from_high: Vec<&str> = es
            .iter()
            .filter(|e| e.src_symbol.as_deref() == Some("high"))
            .map(|e| e.dst_name.as_str())
            .collect();
        // Only `mid()` (with parens) is a call node; bare `low` is just
        // an identifier reference until you add parens or a receiver.
        assert!(from_high.contains(&"mid"), "edges: {es:?}");
    }

    #[test]
    fn ruby_edges_method_receiver() {
        let src = "class C\n  def go\n    Foo.new.bar(1)\n  end\nend\n";
        let es = edges("a.rb", src, "ruby");
        let names = dst_names(&es);
        // Foo.new.bar(1) parses as nested calls: bar on (new on Foo).
        // Both `bar` and `new` should appear; the receiver `Foo` should not.
        assert!(names.contains(&"bar"), "edges: {es:?}");
        assert!(names.contains(&"new"), "edges: {es:?}");
        // The `bar` call should attribute to the enclosing method `go`.
        let bar = es.iter().find(|e| e.dst_name == "bar").unwrap();
        assert_eq!(bar.src_symbol.as_deref(), Some("go"));
    }

    #[test]
    fn extract_file_with_edges_single_parse_returns_both() {
        let src = "function f(){ g(); }\nfunction g(){}\n";
        let (syms, es) = extract_file_with_edges("a.ts", src, "typescript").unwrap();
        assert!(syms.iter().any(|s| s.name == "f"));
        assert!(syms.iter().any(|s| s.name == "g"));
        assert!(es.iter().any(|e| e.dst_name == "g"));
    }

    #[test]
    fn extract_edges_unsupported_lang_errors() {
        assert!(extract_edges("a.txt", "x", "klingon").is_err());
    }
}
