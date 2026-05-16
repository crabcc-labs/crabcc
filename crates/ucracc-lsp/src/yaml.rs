//! YAML extractor. Surfaces top-level keys (and one level of nesting)
//! as DocumentSymbols. Useful for navigating GitHub Actions workflows,
//! Kubernetes manifests, docker-compose files, etc.
//!
//! No call edges — YAML isn't code; references/callHierarchy would be
//! meaningless. documentSymbol + workspaceSymbol still work.

use anyhow::{anyhow, Result};
use crabcc_core::{Edge, Symbol, SymbolKind};
use tree_sitter::{Node, Parser};

pub fn extract(file: &str, src: &str) -> Result<(Vec<Symbol>, Vec<Edge>)> {
    let mut parser = Parser::new();
    let language = tree_sitter_yaml::LANGUAGE.into();
    parser
        .set_language(&language)
        .map_err(|e| anyhow!("set_language(yaml): {e}"))?;
    let tree = parser
        .parse(src, None)
        .ok_or_else(|| anyhow!("yaml parse failed for {file}"))?;
    let mut symbols = Vec::new();
    walk(
        tree.root_node(),
        src.as_bytes(),
        file,
        None,
        0,
        &mut symbols,
    );
    Ok((symbols, Vec::new()))
}

const MAX_DEPTH: u32 = 2;

fn walk(
    node: Node,
    src: &[u8],
    file: &str,
    parent: Option<&str>,
    depth: u32,
    out: &mut Vec<Symbol>,
) {
    if node.kind() == "block_mapping_pair" || node.kind() == "flow_pair" {
        let key_node = node.child_by_field_name("key");
        let key_text = key_node
            .as_ref()
            .and_then(|n| slice(n, src))
            .map(|s| s.trim().trim_matches('"').to_string());
        if let Some(name) = key_text {
            if !name.is_empty() && depth < MAX_DEPTH {
                out.push(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::Var,
                    signature: None,
                    parent: parent.map(str::to_string),
                    file: file.to_string(),
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    visibility: None,
                });
                // Recurse into the value side with this key as parent.
                if let Some(value) = node.child_by_field_name("value") {
                    let mut cursor = value.walk();
                    for child in value.children(&mut cursor) {
                        walk(child, src, file, Some(name.as_str()), depth + 1, out);
                    }
                }
                return;
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, file, parent, depth, out);
    }
}

fn slice<'a>(node: &Node, src: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(src.get(node.start_byte()..node.end_byte())?).ok()
}
