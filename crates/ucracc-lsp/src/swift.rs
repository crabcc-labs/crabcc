//! Swift extractor. crabcc-core's grammar fleet doesn't cover Swift, so
//! we carry our own tree-sitter parser here. Symbols extracted match the
//! shape used by crabcc-core (`Symbol { name, kind, signature, parent,
//! file, line_start, line_end, visibility }`) and are written into the
//! same `.crabcc/index.db` with `language = "swift"`.

use anyhow::{anyhow, Result};
use crabcc_core::{Edge, Symbol, SymbolKind};
use tree_sitter::{Node, Parser};

pub fn extract(file: &str, src: &str) -> Result<(Vec<Symbol>, Vec<Edge>)> {
    let mut parser = Parser::new();
    let language = tree_sitter_swift::LANGUAGE.into();
    parser
        .set_language(&language)
        .map_err(|e| anyhow!("set_language(swift): {e}"))?;
    let tree = parser
        .parse(src, None)
        .ok_or_else(|| anyhow!("swift parse failed for {file}"))?;
    let mut symbols = Vec::new();
    let mut edges = Vec::new();
    walk(tree.root_node(), src.as_bytes(), file, None, &mut symbols, &mut edges);
    Ok((symbols, edges))
}

fn walk(
    node: Node,
    src: &[u8],
    file: &str,
    parent: Option<&str>,
    out: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
) {
    let kind = node.kind();
    let sym_kind = swift_kind(kind);

    let captured_name = if sym_kind.is_some() {
        name_of(&node, src)
    } else {
        None
    };

    if let (Some(sk), Some(name)) = (sym_kind, captured_name.as_deref()) {
        out.push(Symbol {
            name: name.to_string(),
            kind: sk,
            signature: signature_of(&node, src),
            parent: parent.map(str::to_string),
            file: file.to_string(),
            line_start: node.start_position().row as u32 + 1,
            line_end: node.end_position().row as u32 + 1,
            visibility: visibility_of(&node, src),
        });
    }

    if kind == "call_expression" {
        if let Some((callee, line)) = callee_of(&node, src) {
            edges.push(Edge {
                src_file: file.to_string(),
                src_symbol: parent.map(str::to_string),
                dst_name: callee,
                kind: "call".into(),
                line,
            });
        }
    }

    let next_parent = if sym_kind.is_some() {
        captured_name.as_deref().or(parent)
    } else {
        parent
    };

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, file, next_parent, out, edges);
    }
}

fn swift_kind(ts_kind: &str) -> Option<SymbolKind> {
    Some(match ts_kind {
        "function_declaration" => SymbolKind::Function,
        "init_declaration" | "deinit_declaration" => SymbolKind::Method,
        "class_declaration" => SymbolKind::Class,
        "protocol_declaration" => SymbolKind::Interface,
        "enum_declaration" => SymbolKind::Enum,
        "typealias_declaration" => SymbolKind::Type,
        "property_declaration" => SymbolKind::Var,
        _ => return None,
    })
}

fn name_of<'a>(node: &Node<'a>, src: &'a [u8]) -> Option<String> {
    // Initializers and deinitializers don't have a name node — the keyword
    // *is* the identifier as far as call hierarchy and outline are concerned.
    match node.kind() {
        "init_declaration" => return Some("init".to_string()),
        "deinit_declaration" => return Some("deinit".to_string()),
        _ => {}
    }
    if let Some(n) = node.child_by_field_name("name") {
        return slice(&n, src).map(str::to_string);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(child.kind(), "simple_identifier" | "type_identifier") {
            return slice(&child, src).map(str::to_string);
        }
    }
    None
}

fn signature_of(node: &Node, src: &[u8]) -> Option<String> {
    // First line of the declaration is a good-enough hover signature for v1.
    let start = node.start_byte();
    let end = node.end_byte().min(start + 240);
    let bytes = src.get(start..end)?;
    let s = std::str::from_utf8(bytes).ok()?;
    Some(s.lines().next()?.trim().to_string())
}

fn visibility_of(node: &Node, src: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let text = slice(&child, src).unwrap_or("");
            for token in ["public", "open", "internal", "fileprivate", "private"] {
                if text.contains(token) {
                    return Some(token.to_string());
                }
            }
        }
    }
    None
}

fn callee_of(node: &Node, src: &[u8]) -> Option<(String, u32)> {
    // tree-sitter-swift's `call_expression` has no `function` field — the
    // first non-trivia child IS the callee expression. For `foo(x)` that
    // child is a `simple_identifier`. For `obj.bar(x)` it's a
    // `navigation_expression` whose final `navigation_suffix` carries the
    // method name.
    let mut cursor = node.walk();
    let target = node.children(&mut cursor).next()?;
    let (name, line) = match target.kind() {
        "simple_identifier" => (
            slice(&target, src)?.to_string(),
            target.start_position().row as u32 + 1,
        ),
        "navigation_expression" => {
            let mut sub = target.walk();
            // Walk children looking for the last `navigation_suffix`; its
            // simple_identifier child is the method name.
            let mut method: Option<(String, u32)> = None;
            for child in target.children(&mut sub) {
                if child.kind() == "navigation_suffix" {
                    let mut k = child.walk();
                    for grand in child.children(&mut k) {
                        if grand.kind() == "simple_identifier" {
                            method = Some((
                                slice(&grand, src)?.to_string(),
                                grand.start_position().row as u32 + 1,
                            ));
                        }
                    }
                }
            }
            method?
        }
        _ => (
            slice(&target, src)?.to_string(),
            target.start_position().row as u32 + 1,
        ),
    };
    Some((name, line))
}

fn slice<'a>(node: &Node, src: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(src.get(node.start_byte()..node.end_byte())?).ok()
}
