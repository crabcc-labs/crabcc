# Task 3 — resolve.rs: Resolver trait, ScopeCtx, SymbolId, NameOnlyResolver

## Context

v4.0 introduces a `Resolver` trait so the extractor's pass-2 can convert a
use-site name into the `symbol_id` of its definition. Per-language modules
(`extract/resolve_rust.rs`, `extract/resolve_ts.rs`, `extract/resolve_python.rs`)
will implement this trait in later tasks. Until they do, the extractor falls
back to `NameOnlyResolver`, which always returns `None` — meaning every edge
gets routed through `Store::upsert_unresolved_sentinel`, preserving the old
name-based query surface for languages without a real resolver yet
(Ruby/Java/Swift, and Rust/TS/Python until tasks 5–7 land).

This task creates the module and wires it into `lib.rs`. Nothing in the
extractor or store calls `Resolver` yet — that's Task 4's job.

## What to change

### File 1: `crates/crabcc-core/src/resolve.rs` (NEW)

Create the file with this exact content:

```rust
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
    pub raw: String,
    pub alias: Option<String>,
    pub from_module: Option<String>,
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
```

### File 2: `crates/crabcc-core/src/lib.rs` — add one module declaration

Find this exact block (around lines 78–80):

```rust
pub mod query;
pub mod refs;
pub mod service_discovery;
```

Replace it with:

```rust
pub mod query;
pub mod refs;
pub mod resolve;
pub mod service_discovery;
```

That is the only change to `lib.rs`. Do not touch the doc comment block at
the top of the file, do not touch the module table, do not add `resolve` to
the prelude `pub use`. The Resolver trait and friends are referenced through
the fully-qualified path (`crate::resolve::Resolver`, etc.) by other modules.

## Notes for the implementer

- Do not add a `pub use resolve::...;` re-export. Callers (the extractor, the
  per-language resolver modules) reach in via `crate::resolve`.
- The `Send + Sync` bound on `Resolver` is required: the indexer runs
  per-file extraction on a Rayon thread pool, and the resolver is shared by
  reference across worker threads.
- `SymbolId` deliberately has no `From<i64>`/`Into<i64>` impls — keep the
  conversions explicit (`SymbolId(id)` / `id.0`). One less footgun when
  `dst_symbol_id` and `src_symbol_id` are both raw i64 in SQL bindings.

Do not run `cargo build`, `cargo test`, or any other build or test command.

Do not modify any other file. Do not invent extra files.

Then commit with this exact message:

    feat(resolve): Resolver trait, ScopeCtx, SymbolId, NameOnlyResolver fallback
