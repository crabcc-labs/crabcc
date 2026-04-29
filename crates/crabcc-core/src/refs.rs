// Identifier-reference walker via raw tree-sitter.
//
// Used for `crabcc refs <name>` — finds every identifier whose text equals
// `name`, regardless of usage context (calls, type annotations, imports, etc.).
// Coarser than ast-grep's pattern-match (no semantics), but cheap and broad.

use crate::types::Hit;
use anyhow::{anyhow, Result};
use tree_sitter::{Node, Parser};

pub fn find_refs(src: &str, lang: &str, name: &str) -> Result<Vec<Hit>> {
    let ts_lang = match lang {
        "typescript" => tree_sitter_typescript::language_typescript(),
        "tsx"        => tree_sitter_typescript::language_tsx(),
        "javascript" => tree_sitter_javascript::language(),
        "ruby"       => tree_sitter_ruby::language(),
        _ => return Err(anyhow!("unsupported lang: {lang}")),
    };
    let mut parser = Parser::new();
    parser.set_language(&ts_lang).map_err(|e| anyhow!("set_language: {e}"))?;
    let tree = parser.parse(src, None).ok_or_else(|| anyhow!("parse failed"))?;

    let mut out = Vec::new();
    walk(tree.root_node(), src.as_bytes(), src, name, lang, &mut out);
    Ok(out)
}

fn walk(node: Node, src_bytes: &[u8], src_full: &str, name: &str, lang: &str, out: &mut Vec<Hit>) {
    if is_identifier_kind(lang, node.kind()) {
        if let Ok(text) = node.utf8_text(src_bytes) {
            if text == name {
                let p = node.start_position();
                out.push(Hit {
                    file: String::new(),
                    line: (p.row + 1) as u32,
                    col:  (p.column + 1) as u32,
                    snippet: line_at(src_full, p.row),
                });
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src_bytes, src_full, name, lang, out);
    }
}

fn is_identifier_kind(lang: &str, kind: &str) -> bool {
    match lang {
        "typescript" | "tsx" | "javascript" => matches!(
            kind,
            "identifier"
                | "type_identifier"
                | "property_identifier"
                | "shorthand_property_identifier"
                | "shorthand_property_identifier_pattern"
        ),
        "ruby" => matches!(
            kind,
            "identifier" | "constant" | "class_variable" | "instance_variable" | "global_variable"
        ),
        _ => false,
    }
}

fn line_at(src: &str, row: usize) -> String {
    src.lines()
        .nth(row)
        .map(|l| l.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_refs_in_expression() {
        let src = r#"
const greeted = greet("hi");
function greet(s: string) { return s; }
const again = greet("again");
"#;
        let hits = find_refs(src, "typescript", "greet").unwrap();
        // greet is referenced 3 times: definition + 2 calls.
        assert!(hits.len() >= 3, "got: {hits:?}");
    }

    #[test]
    fn ruby_refs_to_constant() {
        let src = "class User\nend\nUser.new\nUser.find(1)\n";
        let hits = find_refs(src, "ruby", "User").unwrap();
        assert!(hits.len() >= 3, "got: {hits:?}");
    }

    #[test]
    fn unsupported_lang_errors() {
        assert!(find_refs("x", "klingon", "x").is_err());
    }

    #[test]
    fn no_match_returns_empty() {
        let hits = find_refs("function foo() {}", "typescript", "bar").unwrap();
        assert_eq!(hits.len(), 0);
    }
}
