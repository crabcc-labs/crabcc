use crate::types::{Symbol, SymbolKind};
use anyhow::{anyhow, Result};
use std::path::Path;
use tree_sitter::{Node, Parser};

pub fn detect_lang(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?;
    Some(match ext {
        "ts" => "typescript",
        "tsx" => "tsx",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "rb" | "rake" | "gemspec" => "ruby",
        _ => return None,
    })
}

pub fn extract_file(file: &str, src: &str, lang: &str) -> Result<Vec<Symbol>> {
    let ts_lang = match lang {
        "typescript" => tree_sitter_typescript::language_typescript(),
        "tsx" => tree_sitter_typescript::language_tsx(),
        "javascript" => tree_sitter_javascript::language(),
        "ruby" => tree_sitter_ruby::language(),
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
    walk(tree.root_node(), src.as_bytes(), file, lang, None, &mut out);
    Ok(out)
}

fn walk(
    node: Node,
    src: &[u8],
    file: &str,
    lang: &str,
    parent: Option<&str>,
    out: &mut Vec<Symbol>,
) {
    if let Some(kind) = symbol_kind_for(lang, node.kind()) {
        if let Some(name) = node_name(&node, src) {
            let n_owned = name.to_string();
            let line_start = (node.start_position().row + 1) as u32;
            let line_end = (node.end_position().row + 1) as u32;
            out.push(Symbol {
                name: n_owned.clone(),
                kind,
                signature: signature_for(&node, src, lang),
                parent: parent.map(String::from),
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

fn node_name<'a>(node: &Node, src: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
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

    #[test]
    fn unsupported_lang_errors() {
        assert!(extract_file("a.txt", "hello", "klingon").is_err());
    }
}
