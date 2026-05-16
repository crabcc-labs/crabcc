//! Symbol resolution for the v4 two-pass extractor.
//!
//! Pass 1 of `extract_file` calls `Store::insert_symbol` for every definition
//! and collects the returned rowids into a `HashMap<String, SymbolId>`.
//! Pass 2 walks every use / call site, builds a [`ScopeCtx`], and asks the
//! per-language [`Resolver`] to translate the use-site name into a
//! [`SymbolId`]. On `Some(id)` the extractor emits an edge with that id as
//! `dst_symbol_id`; on `None` it routes through
//! `Store::upsert_unresolved_sentinel` instead (preserving the old
//! name-based recall for languages without a real resolver yet).
//!
//! [`NameOnlyResolver`] is the fallback used by both languages whose
//! resolver hasn't been implemented yet and by tests that want sentinel
//! behaviour deterministically.

use std::collections::HashMap;

/// Newtype around a `symbols.id` (i64). Hands-off — the resolver receives
/// these from pass 1 and hands them back to pass 2 unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub i64);

/// One ES/Python/Rust import the extractor saw at file scope. The resolver
/// uses this list to map a bare name (e.g. `Foo` in `let x: Foo = ...`)
/// back to its source module before looking it up.
///
/// - `raw`: the imported name as it appears at the import site
///   (`Foo` in `use bar::Foo;`, `useState` in `import { useState } from "react";`).
/// - `alias`: the bound local name when it differs from `raw`
///   (`F` in `use bar::Foo as F;`).
/// - `from_module`: the module path the symbol comes from, when known
///   (`"bar"` for `use bar::Foo`, `"react"` for ES imports).
#[derive(Debug, Clone)]
pub struct ImportSpec {
    /// Locally-visible name (`Foo` in `use bar::Foo`, the imported symbol's
    /// in-scope handle). Resolvers match against this on use-site lookups.
    pub local: String,
    /// Fully-qualified path (`bar::Foo` for the same example). Resolvers
    /// pass this to `SymbolIndex::lookup_qualified` for cross-file lookups.
    pub qualified: String,
}

/// Per-file scope context passed to every resolver call. Borrows from
/// pass-1 outputs so the resolver never has to re-walk the tree.
///
/// `local_defs` keys are the bare definition names from pass 1 — the same
/// strings the resolver will see at use sites. Multiple defs with the same
/// name in the same file shadow each other and the last one wins; that
/// matches what the SQL-level lookup would do for an ambiguous query.
pub struct ScopeCtx<'a> {
    pub file_id: i64,
    pub current_module: Option<&'a str>,
    pub imports: &'a [ImportSpec],
    pub local_defs: &'a HashMap<String, SymbolId>,
}

/// Per-language symbol resolution. Implementors do their own scope walking
/// (import resolution, method dispatch heuristics, etc.) and return either
/// the resolved `symbols.id` or `None` to let the extractor fall back to
/// the sentinel pattern.
pub trait Resolver: Send + Sync {
    /// Resolve a use-site name (`Foo` in `let x: Foo = ...`) to a definition.
    /// `None` => the extractor will emit an unresolved-name edge via the
    /// sentinel pattern.
    fn resolve_ref(&self, scope: &ScopeCtx, name: &str) -> Option<SymbolId>;

    /// Resolve a call-site callee (`foo()` / `obj.method()` / `Type::assoc()`).
    /// `None` => sentinel fallback, same as `resolve_ref`.
    fn resolve_call(&self, scope: &ScopeCtx, callee: &str) -> Option<SymbolId>;
}

/// Fallback resolver: always returns `None`. Use this for languages whose
/// real resolver hasn't been implemented yet (Ruby, Java, Swift) and for
/// any path where we want sentinel behaviour deterministically. The
/// extractor will route every edge through `upsert_unresolved_sentinel`,
/// preserving the old name-based query surface.
pub struct NameOnlyResolver;

/// Cross-file symbol index queried by the per-language resolvers during
/// pass 2. The extractor passes an `Arc<dyn SymbolIndex>` so resolvers
/// can resolve `Foo::method` (where `Foo` lives in a different file) to
/// a concrete `SymbolId`. Store-backed impl lands when the extractor
/// is wired to pass it in; resolvers carry stubs in tests.
pub trait SymbolIndex: Send + Sync {
    /// Look up by fully-qualified name (e.g. "crate::module::Foo").
    fn lookup_qualified(&self, qualified: &str) -> Option<SymbolId>;
    /// Look up by bare name; returns all candidates across files.
    fn lookup_by_name(&self, name: &str) -> Vec<SymbolId>;
}

impl Resolver for NameOnlyResolver {
    fn resolve_ref(&self, _scope: &ScopeCtx, _name: &str) -> Option<SymbolId> {
        None
    }

    fn resolve_call(&self, _scope: &ScopeCtx, _callee: &str) -> Option<SymbolId> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_scope() -> (HashMap<String, SymbolId>, Vec<ImportSpec>) {
        (HashMap::new(), Vec::new())
    }

    #[test]
    fn name_only_resolver_returns_none_for_refs() {
        let (defs, imports) = empty_scope();
        let scope = ScopeCtx {
            file_id: 1,
            current_module: None,
            imports: &imports,
            local_defs: &defs,
        };
        assert!(NameOnlyResolver.resolve_ref(&scope, "Foo").is_none());
    }

    #[test]
    fn name_only_resolver_returns_none_for_calls() {
        let mut defs = HashMap::new();
        defs.insert("foo".to_string(), SymbolId(42));
        let imports = vec![ImportSpec {
            raw: "Bar".into(),
            alias: None,
            from_module: Some("baz".into()),
        }];
        let scope = ScopeCtx {
            file_id: 7,
            current_module: Some("mymod"),
            imports: &imports,
            local_defs: &defs,
        };
        // Even with non-empty defs and imports, NameOnlyResolver ignores them
        // and always returns None. The whole point is to force sentinel
        // fallback for languages without a real resolver.
        assert!(NameOnlyResolver.resolve_call(&scope, "foo").is_none());
        assert!(NameOnlyResolver.resolve_call(&scope, "Bar").is_none());
    }
}
