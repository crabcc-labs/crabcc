//! Markdown extractor. Surfaces ATX (`# foo`) and setext (`foo\n===`)
//! headings as a navigable doc outline. H1 -> Class, H2 -> Module,
//! H3+ -> Function in LSP SymbolKind terms — picked so editor outline
//! views render the hierarchy with sensible icons.
//!
//! Uses only the *block* grammar from tree-sitter-md. Inline contents
//! of headings (bold, links, etc.) are returned as raw markdown — good
//! enough for nav.

use anyhow::{anyhow, Result};
use crabcc_core::{Edge, Symbol, SymbolKind};
use tree_sitter::{Node, Parser};

pub fn extract(file: &str, src: &str) -> Result<(Vec<Symbol>, Vec<Edge>)> {
    let mut parser = Parser::new();
    let language = tree_sitter_md::LANGUAGE.into();
    parser
        .set_language(&language)
        .map_err(|e| anyhow!("set_language(markdown): {e}"))?;
    let tree = parser
        .parse(src, None)
        .ok_or_else(|| anyhow!("markdown parse failed for {file}"))?;
    let mut symbols = Vec::new();
    walk(tree.root_node(), src.as_bytes(), file, &mut symbols);
    Ok((symbols, Vec::new()))
}

fn walk(node: Node, src: &[u8], file: &str, out: &mut Vec<Symbol>) {
    let kind = node.kind();
    if kind == "atx_heading" {
        if let Some((level, text)) = atx_heading(&node, src) {
            out.push(make_symbol(text, level, &node, file));
        }
    } else if kind == "setext_heading" {
        if let Some((level, text)) = setext_heading(&node, src) {
            out.push(make_symbol(text, level, &node, file));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, file, out);
    }
}

fn make_symbol(text: String, level: u32, node: &Node, file: &str) -> Symbol {
    let kind = match level {
        1 => SymbolKind::Class,
        2 => SymbolKind::Type,
        _ => SymbolKind::Function,
    };
    Symbol {
        name: text,
        kind,
        signature: None,
        parent: None,
        file: file.to_string(),
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        visibility: None,
    }
}

fn atx_heading(node: &Node, src: &[u8]) -> Option<(u32, String)> {
    let mut level = 0u32;
    let mut text: Option<String> = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            k if k.starts_with("atx_h") && k.ends_with("_marker") => {
                // atx_h1_marker .. atx_h6_marker
                level = k
                    .trim_start_matches("atx_h")
                    .trim_end_matches("_marker")
                    .parse()
                    .unwrap_or(0);
            }
            "inline" => {
                text = slice(&child, src).map(|s| s.trim().to_string());
            }
            _ => {}
        }
    }
    let text = text.filter(|s| !s.is_empty())?;
    if level == 0 {
        return None;
    }
    Some((level, text))
}

fn setext_heading(node: &Node, src: &[u8]) -> Option<(u32, String)> {
    let mut level = 0u32;
    let mut text: Option<String> = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "setext_h1_underline" => level = 1,
            "setext_h2_underline" => level = 2,
            "paragraph" => {
                text = slice(&child, src).map(|s| s.trim().to_string());
            }
            _ => {}
        }
    }
    let text = text.filter(|s| !s.is_empty())?;
    if level == 0 {
        return None;
    }
    Some((level, text))
}

fn slice<'a>(node: &Node, src: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(src.get(node.start_byte()..node.end_byte())?).ok()
}
