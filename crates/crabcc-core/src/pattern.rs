// Pattern-based queries via ast-grep.
//
// Used for `crabcc callers <name>` — finds all `name(...)` call sites.
// `refs` (any identifier reference) goes through the lighter-weight
// tree-sitter walker in `refs.rs`.

use crate::types::Hit;
#[cfg(test)]
use anyhow::Result;
use ast_grep_core::{AstGrep, Pattern};
use ast_grep_language::SupportLang;
use std::collections::HashSet;

pub fn lang_for(s: &str) -> Option<SupportLang> {
    Some(match s {
        "typescript" => SupportLang::TypeScript,
        "tsx" => SupportLang::Tsx,
        "javascript" => SupportLang::JavaScript,
        "ruby" => SupportLang::Ruby,
        "rust" => SupportLang::Rust,
        "go" => SupportLang::Go,
        "python" => SupportLang::Python,
        _ => return None,
    })
}

/// Smoke test that the ast-grep crates are wired correctly. `pub(crate)` so
/// other modules can call it during their own tests, but not part of the
/// public surface.
#[cfg(test)]
pub(crate) fn smoke(lang: SupportLang, src: &str) -> Result<()> {
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
    let bare = format!("{name}($$$)");
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
                col: (col + 1) as u32,
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
        assert!(matches!(
            lang_for("typescript"),
            Some(SupportLang::TypeScript)
        ));
        assert!(matches!(lang_for("ruby"), Some(SupportLang::Ruby)));
        assert!(matches!(lang_for("rust"), Some(SupportLang::Rust)));
        assert!(matches!(lang_for("go"), Some(SupportLang::Go)));
        assert!(matches!(lang_for("python"), Some(SupportLang::Python)));
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
        assert!(
            hits.len() >= 2,
            "expected ≥2 receiver bar() calls, got: {hits:?}"
        );
    }

    #[test]
    fn smoke_rust_parses() {
        smoke(SupportLang::Rust, "fn x() {}").unwrap();
    }

    #[test]
    fn callers_rust_bare_and_method() {
        let src = "fn greet(n: &str) {}\nfn main() { greet(\"world\"); foo.greet(\"there\"); }\n";
        let hits = find_callers(src, SupportLang::Rust, "greet");
        assert!(hits.len() >= 2, "expected ≥2 greet calls, got: {hits:?}");
    }

    #[test]
    fn smoke_go_parses() {
        smoke(SupportLang::Go, "package x\nfunc x() {}").unwrap();
    }

    #[test]
    fn callers_go_bare_call() {
        // Go bare-call detection works through the same `name($$$)` pattern as
        // every other lang. Receiver-form calls (`u.Greet(...)`) currently only
        // match in some grammars; we don't assert on those for Go (tracked in
        // the v2.0 epic — improving cross-language pattern coverage).
        let src = "package x\nfunc Greet(n string) {}\nfunc main() {\n  Greet(\"world\")\n  Greet(\"again\")\n}\n";
        let hits = find_callers(src, SupportLang::Go, "Greet");
        assert!(hits.len() >= 2, "expected ≥2 Greet calls, got: {hits:?}");
    }

    #[test]
    fn smoke_python_parses() {
        smoke(SupportLang::Python, "def x():\n    pass\n").unwrap();
    }

    #[test]
    fn callers_python_bare_and_method() {
        let src = "def greet(n):\n    pass\n\ngreet('world')\nuser.greet('there')\n";
        let hits = find_callers(src, SupportLang::Python, "greet");
        assert!(hits.len() >= 2, "expected ≥2 greet calls, got: {hits:?}");
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
    fn callers_rejects_empty_name() {
        // Boundary: empty needle should not panic and should return zero hits
        // (an empty identifier never matches a real call site).
        let hits = find_callers("foo()", SupportLang::TypeScript, "");
        assert!(hits.is_empty());
    }

    #[test]
    fn callers_dedups_overlapping_pattern_matches() {
        // The bare `name($$$)` and `$RECV.name($$$)` patterns can both fire on
        // the same syntax position depending on how ast-grep walks the tree.
        // We dedup by (line, col) — verify a bare call doesn't double-count.
        let src = "function greet(){} greet();";
        let hits = find_callers(src, SupportLang::TypeScript, "greet");
        assert_eq!(hits.len(), 1, "expected exactly 1 hit, got: {hits:?}");
    }

    #[test]
    fn callers_returns_sorted_hits() {
        // Result ordering matters for `--limit N` to be deterministic across
        // runs. We sort by (line, col) at the end of find_callers.
        let src = "function f(){}\nf();\nf();\nf();\n";
        let hits = find_callers(src, SupportLang::TypeScript, "f");
        assert!(hits.len() >= 2);
        for w in hits.windows(2) {
            assert!(
                (w[0].line, w[0].col) <= (w[1].line, w[1].col),
                "hits not sorted: {hits:?}"
            );
        }
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

    #[test]
    fn snippet_under_80_chars_passes_through() {
        let s = compact_snippet("hello world");
        assert_eq!(s, "hello world");
        assert!(!s.ends_with('…'));
    }

    #[test]
    fn snippet_over_80_chars_truncates_with_ellipsis() {
        let long = "a ".repeat(100); // 200 chars of "a a a …"
        let s = compact_snippet(&long);
        // We collapse whitespace then cap at 80 chars + "…".
        assert!(s.ends_with('…'), "got: {s:?}");
        // First 80 chars should be the truncated body.
        let body = s.trim_end_matches('…');
        assert_eq!(
            body.chars().count(),
            80,
            "body chars: {}",
            body.chars().count()
        );
    }

    #[test]
    fn snippet_collapses_internal_whitespace() {
        let s = compact_snippet("foo\n\tbar    baz");
        assert_eq!(s, "foo bar baz");
    }
}
