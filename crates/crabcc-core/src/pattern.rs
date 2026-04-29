// Pattern-based queries via ast-grep.
//
// Used for `crabcc callers <name>` — finds all `name(...)` call sites.
// `refs` (any identifier reference) goes through the lighter-weight
// tree-sitter walker in `refs.rs`.

use crate::types::Hit;
use anyhow::Result;
use ast_grep_core::{AstGrep, Pattern};
use ast_grep_language::SupportLang;
use std::collections::HashSet;

pub fn lang_for(s: &str) -> Option<SupportLang> {
    Some(match s {
        "typescript" => SupportLang::TypeScript,
        "tsx"        => SupportLang::Tsx,
        "javascript" => SupportLang::JavaScript,
        "ruby"       => SupportLang::Ruby,
        _ => return None,
    })
}

/// Smoke test that the ast-grep crates are wired correctly.
pub fn smoke(lang: SupportLang, src: &str) -> Result<()> {
    let grep = AstGrep::new(src, lang);
    let _root = grep.root();
    Ok(())
}

/// Find call sites of `name` in `src`.
/// Tries both patterns — bare-receiver `name($$$)` and explicit-receiver
/// `$RECV.name($$$)` — and unions the hits, deduped by (line, col).
/// This catches the Ruby/JS distinction between `foo()` and `obj.foo()`.
pub fn find_callers(src: &str, lang: SupportLang, name: &str) -> Vec<Hit> {
    if !is_safe_identifier(name) {
        return Vec::new();
    }
    let bare   = format!("{name}($$$)");
    let method = format!("$RECV.{name}($$$)");
    let grep = AstGrep::new(src, lang);
    let root = grep.root();

    let mut out: Vec<Hit> = Vec::new();
    let mut seen: HashSet<(usize, usize)> = HashSet::new();

    for pattern_src in [bare.as_str(), method.as_str()] {
        let pattern = Pattern::new(pattern_src, lang);
        for m in root.find_all(pattern) {
            let n = m.get_node();
            let (line, col) = n.start_pos();
            if !seen.insert((line, col)) {
                continue;
            }
            out.push(Hit {
                file: String::new(),
                line: (line + 1) as u32,
                col:  (col + 1) as u32,
                snippet: compact_snippet(n.text().as_ref()),
            });
        }
    }
    out.sort_by_key(|h| (h.line, h.col));
    out
}

fn is_safe_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !s.chars().next().unwrap().is_ascii_digit()
}

fn compact_snippet(s: &str) -> String {
    let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    // 80 chars is enough to disambiguate a call site without paying for noise.
    if one_line.len() > 80 {
        format!("{}…", &one_line[..80])
    } else {
        one_line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_for_known() {
        assert!(matches!(lang_for("typescript"), Some(SupportLang::TypeScript)));
        assert!(matches!(lang_for("ruby"),       Some(SupportLang::Ruby)));
        assert!(lang_for("klingon").is_none());
    }

    #[test]
    fn smoke_typescript_parses() {
        smoke(SupportLang::TypeScript, "const x = 1;").unwrap();
    }

    #[test]
    fn smoke_ruby_parses() {
        smoke(SupportLang::Ruby, "class Foo; end").unwrap();
    }

    #[test]
    fn callers_typescript_simple() {
        let src = r#"
function greet(n: string) { return "hi " + n; }
greet("world");
greet("there");
"#;
        let hits = find_callers(src, SupportLang::TypeScript, "greet");
        assert_eq!(hits.len(), 2, "got: {hits:?}");
        assert!(hits[0].snippet.starts_with("greet("));
    }

    #[test]
    fn callers_ruby_bare_call() {
        let src = "def bar(x); x; end\nbar(1)\nbar(2)\n";
        let hits = find_callers(src, SupportLang::Ruby, "bar");
        assert!(hits.len() >= 2, "expected ≥2 bar() calls, got: {hits:?}");
    }

    #[test]
    fn callers_ruby_with_receiver() {
        // The `$RECV.bar($$$)` pattern catches method-receiver calls,
        // which are the dominant Ruby/Rails shape.
        let src = "Foo.new.bar(1)\nFoo.new.bar(2)\n";
        let hits = find_callers(src, SupportLang::Ruby, "bar");
        assert!(hits.len() >= 2, "expected ≥2 receiver bar() calls, got: {hits:?}");
    }

    #[test]
    fn callers_typescript_method_call() {
        let src = "obj.greet('hi'); other.greet('there');";
        let hits = find_callers(src, SupportLang::TypeScript, "greet");
        assert_eq!(hits.len(), 2, "got: {hits:?}");
    }

    #[test]
    fn callers_rejects_invalid_name() {
        let hits = find_callers("foo()", SupportLang::TypeScript, "x-y");
        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn safe_identifier_check() {
        assert!(is_safe_identifier("foo"));
        assert!(is_safe_identifier("_priv"));
        assert!(is_safe_identifier("Foo123"));
        assert!(!is_safe_identifier(""));
        assert!(!is_safe_identifier("1foo"));
        assert!(!is_safe_identifier("foo-bar"));
        assert!(!is_safe_identifier("foo bar"));
    }
}
