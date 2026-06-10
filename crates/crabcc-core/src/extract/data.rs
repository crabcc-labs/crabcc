//! Extractors for data-shaped formats: Markdown, YAML, CSV.
//!
//! These don't fit the generic tree-sitter dispatch tables in `mod.rs`
//! (`symbol_kind_for` + `node_name`) — Markdown headings and YAML keys
//! carry their names in format-specific node shapes, and CSV has no
//! grammar at all. Each format gets a dedicated walker instead; none of
//! them emit call edges (there is nothing call-shaped to extract).
//!
//! Markdown surfaces ATX (`# foo`) and setext (`foo\n===`) headings as a
//! navigable doc outline: H1 -> Class, H2 -> Type, H3+ -> Function —
//! same mapping ucracc-lsp established so outline views render a
//! sensible hierarchy. YAML surfaces top-level keys plus one level of
//! nesting (GitHub Actions workflows, docker-compose, k8s manifests).
//! CSV surfaces header-row columns.

use crate::types::{Symbol, SymbolKind};
use anyhow::{anyhow, Result};
use tree_sitter::Node;

pub(crate) fn is_data_lang(lang: &str) -> bool {
    matches!(lang, "markdown" | "yaml" | "csv")
}

/// Symbols-only extraction for data formats. Markdown/YAML go through the
/// shared per-thread parser pool; CSV is a plain line scan.
pub(crate) fn extract(file: &str, src: &str, lang: &str) -> Result<Vec<Symbol>> {
    match lang {
        "markdown" | "yaml" => super::with_parser(lang, |parser| {
            let tree = parser
                .parse(src, None)
                .ok_or_else(|| anyhow!("parse failed"))?;
            let mut out = Vec::new();
            extract_from_node(tree.root_node(), src.as_bytes(), file, lang, &mut out);
            Ok(out)
        }),
        "csv" => Ok(csv_header_symbols(file, src)),
        _ => Err(anyhow!("not a data lang: {lang}")),
    }
}

/// Walk an already-parsed Markdown/YAML tree (the `extract_from_root` path,
/// for consumers that own their `Parser`). CSV never has a tree, so it has
/// no arm here.
pub(crate) fn extract_from_node(
    root: Node,
    src: &[u8],
    file: &str,
    lang: &str,
    out: &mut Vec<Symbol>,
) {
    match lang {
        "markdown" => walk_markdown(root, src, file, out),
        "yaml" => walk_yaml(root, src, file, None, 0, out),
        _ => {}
    }
}

// ---- Markdown ----

fn walk_markdown(node: Node, src: &[u8], file: &str, out: &mut Vec<Symbol>) {
    match node.kind() {
        "atx_heading" => {
            if let Some((level, text)) = atx_heading(&node, src) {
                out.push(heading_symbol(text, level, &node, file));
            }
        }
        "setext_heading" => {
            if let Some((level, text)) = setext_heading(&node, src) {
                out.push(heading_symbol(text, level, &node, file));
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_markdown(child, src, file, out);
    }
}

fn heading_symbol(text: String, level: u32, node: &Node, file: &str) -> Symbol {
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
            // atx_h1_marker .. atx_h6_marker
            k if k.starts_with("atx_h") && k.ends_with("_marker") => {
                level = k
                    .trim_start_matches("atx_h")
                    .trim_end_matches("_marker")
                    .parse()
                    .unwrap_or_default();
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

// ---- YAML ----

/// Top-level keys + one level of nesting. Deeper nesting (a k8s manifest's
/// `spec.template.spec.containers...`) floods the index without aiding nav.
const YAML_MAX_DEPTH: u32 = 2;

fn walk_yaml(
    node: Node,
    src: &[u8],
    file: &str,
    parent: Option<&str>,
    depth: u32,
    out: &mut Vec<Symbol>,
) {
    if node.kind() == "block_mapping_pair" || node.kind() == "flow_pair" {
        let key_text = node
            .child_by_field_name("key")
            .as_ref()
            .and_then(|n| slice(n, src))
            .map(|s| s.trim().trim_matches('"').to_string());
        if let Some(name) = key_text {
            if !name.is_empty() && depth < YAML_MAX_DEPTH {
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
                        walk_yaml(child, src, file, Some(name.as_str()), depth + 1, out);
                    }
                }
                return;
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_yaml(child, src, file, parent, depth, out);
    }
}

// ---- CSV ----

/// Column cap for pathological headers (machine-generated wide tables).
/// 256 named columns is already beyond what an agent navigates by name.
const CSV_MAX_COLUMNS: usize = 256;

/// One symbol per header-row column. The first non-empty line is taken as
/// the header; files without one (or with only blank lines) yield nothing.
fn csv_header_symbols(file: &str, src: &str) -> Vec<Symbol> {
    let Some((idx, header)) = src.lines().enumerate().find(|(_, l)| !l.trim().is_empty()) else {
        return Vec::new();
    };
    let line = idx as u32 + 1;
    split_csv_row(header)
        .into_iter()
        .take(CSV_MAX_COLUMNS)
        .filter(|name| !name.is_empty())
        .map(|name| Symbol {
            name,
            kind: SymbolKind::Var,
            signature: None,
            parent: None,
            file: file.to_string(),
            line_start: line,
            line_end: line,
            visibility: None,
        })
        .collect()
}

/// Minimal RFC-4180 field split: commas inside double quotes don't separate,
/// `""` inside a quoted field unescapes to `"`. Anything fancier (embedded
/// newlines in quoted headers) degrades to a truncated name, not a panic.
fn split_csv_row(row: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = row.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                fields.push(std::mem::take(&mut cur).trim().to_string());
            }
            _ => cur.push(c),
        }
    }
    fields.push(cur.trim().to_string());
    fields
}

fn slice<'a>(node: &Node, src: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(src.get(node.start_byte()..node.end_byte())?).ok()
}
