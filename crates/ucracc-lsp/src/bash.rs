//! Bash extractor. Surfaces function definitions and top-level variable
//! assignments. Sufficient for navigating shell scripts in editors.

use anyhow::{anyhow, Result};
use crabcc_core::{Edge, Symbol, SymbolKind};
use tree_sitter::{Node, Parser};

pub fn extract(file: &str, src: &str) -> Result<(Vec<Symbol>, Vec<Edge>)> {
    let mut parser = Parser::new();
    let language = tree_sitter_bash::LANGUAGE.into();
    parser
        .set_language(&language)
        .map_err(|e| anyhow!("set_language(bash): {e}"))?;
    let tree = parser
        .parse(src, None)
        .ok_or_else(|| anyhow!("bash parse failed for {file}"))?;
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
    match node.kind() {
        "function_definition" => {
            let name = node
                .child_by_field_name("name")
                .and_then(|n| slice(&n, src))
                .map(str::to_string);
            if let Some(name) = name {
                out.push(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::Function,
                    signature: signature_of(&node, src),
                    parent: parent.map(str::to_string),
                    file: file.to_string(),
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    visibility: None,
                });
                // Recurse into the function body so nested funcs and
                // command calls still get picked up.
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk(child, src, file, Some(name.as_str()), out, edges);
                }
                return;
            }
        }
        "variable_assignment" => {
            let name = node
                .child_by_field_name("name")
                .and_then(|n| slice(&n, src))
                .map(str::to_string);
            if let Some(name) = name {
                if parent.is_none() {
                    // Only record top-level assignments — inside fn bodies
                    // they'd flood the outline.
                    out.push(Symbol {
                        name,
                        kind: SymbolKind::Var,
                        signature: signature_of(&node, src),
                        parent: None,
                        file: file.to_string(),
                        line_start: node.start_position().row as u32 + 1,
                        line_end: node.end_position().row as u32 + 1,
                        visibility: None,
                    });
                }
            }
        }
        "command" => {
            // `cmd arg arg` — record `cmd` as a call edge so callHierarchy
            // can light up for shell-script callgraphs.
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Some(name) = slice(&name_node, src) {
                    edges.push(Edge {
                        src_file: file.to_string(),
                        src_symbol: parent.map(str::to_string),
                        dst_name: name.to_string(),
                        kind: "call".into(),
                        line: name_node.start_position().row as u32 + 1,
                    });
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, file, parent, out, edges);
    }
}

fn signature_of(node: &Node, src: &[u8]) -> Option<String> {
    let start = node.start_byte();
    let end = node.end_byte().min(start + 200);
    let bytes = src.get(start..end)?;
    let s = std::str::from_utf8(bytes).ok()?;
    Some(s.lines().next()?.trim().to_string())
}

fn slice<'a>(node: &Node, src: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(src.get(node.start_byte()..node.end_byte())?).ok()
}
