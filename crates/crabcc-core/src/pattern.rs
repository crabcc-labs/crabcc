// Pattern-based queries via ast-grep.
//
// ast-grep gives us: pattern matching with metavars (e.g. `Foo($$$)`)
// across the same set of tree-sitter grammars we're already parsing with.
// This module is the planned backend for `crabcc refs` and `crabcc callers`.
//
// v1 ships only a proof-of-life entry that validates the dep + version pin.

use anyhow::Result;
use ast_grep_core::AstGrep;
use ast_grep_language::SupportLang;

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
/// Returns Ok(()) if a parse round-trip succeeds.
pub fn smoke(lang: SupportLang, src: &str) -> Result<()> {
    let grep = AstGrep::new(src, lang);
    let _root = grep.root();
    Ok(())
}

// TODO(crabcc/v1.1): expose pattern matching for refs/callers.
//   pub fn find_pattern(src: &str, lang: SupportLang, pattern: &str)
//       -> Result<Vec<PatternHit>>;

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
}
