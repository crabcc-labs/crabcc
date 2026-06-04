// Identifier-reference walker via raw tree-sitter.
//
// Used for `crabcc refs <name>` — finds every identifier whose text equals
// `name`, regardless of usage context (calls, type annotations, imports, etc.).
// Coarser than ast-grep's pattern-match (no semantics), but cheap and broad.

use crate::types::Hit;
use anyhow::{anyhow, Result};
use tree_sitter::{Node, Parser};

pub fn find_refs(src: &str, lang: &str, name: &str) -> Result<Vec<Hit>> {
    let ts_lang: tree_sitter::Language = match lang {
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        _ => return Err(anyhow!("unsupported lang: {lang}")),
    };
    let mut parser = Parser::new();
    parser
        .set_language(&ts_lang)
        .map_err(|e| anyhow!("set_language: {e}"))?;
    let tree = parser
        .parse(src, None)
        .ok_or_else(|| anyhow!("parse failed"))?;

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
                    col: (p.column + 1) as u32,
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
    let raw = src.lines().nth(row).unwrap_or_default().trim();
    if raw.len() > 80 {
        format!("{}…", &raw[..80])
    } else {
        raw.to_string()
    }
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

    #[test]
    fn tsx_refs_are_found() {
        // TSX uses a separate tree-sitter language; verify it's wired up.
        let src = "const x = greet('hi');\nfunction greet(s: string) { return s; }\n";
        let hits = find_refs(src, "tsx", "greet").unwrap();
        assert!(hits.len() >= 2, "got: {hits:?}");
    }

    #[test]
    fn javascript_refs_are_found() {
        let src = "function hello() { return world(); }\nfunction world() {}\n";
        let hits = find_refs(src, "javascript", "world").unwrap();
        assert!(hits.len() >= 2, "got: {hits:?}");
    }

    #[test]
    fn ruby_instance_variable_refs() {
        let src = "class Foo\n  def init\n    @bar = 1\n  end\n  def show\n    @bar\n  end\nend\n";
        let hits = find_refs(src, "ruby", "@bar").unwrap();
        assert!(hits.len() >= 2, "got: {hits:?}");
    }

    #[test]
    fn hit_carries_correct_line_number() {
        // `greet` on line 2 (1-based) of the source.
        let src = "const x = 1;\nconst y = greet();\nfunction greet() {}\n";
        let hits = find_refs(src, "typescript", "greet").unwrap();
        // At least one hit should have line >= 2.
        assert!(
            hits.iter().any(|h| h.line >= 2),
            "expected a hit at line >=2, got: {hits:?}"
        );
    }

    #[test]
    fn long_line_snippet_is_truncated() {
        // Build a source line that exceeds 80 characters.
        let padding = "x".repeat(100);
        let src = format!("const {} = target();\n", padding);
        let hits = find_refs(&src, "typescript", "target").unwrap();
        assert!(!hits.is_empty(), "no hits: {hits:?}");
        // All snippets must be at most 81 chars (80 + the ellipsis character).
        for h in &hits {
            assert!(
                h.snippet.chars().count() <= 81,
                "snippet too long: {:?}",
                h.snippet
            );
        }
    }
}
