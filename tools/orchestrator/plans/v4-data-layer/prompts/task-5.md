# Task 5 — Rust resolver: use/mod/impl-aware scope walker

## Context

v4.0 promotes the `edges` table to symbol-ID FKs. Each language gets its own
`Resolver` implementation that turns a use-site name into a `SymbolId` (or
`None`, in which case the caller emits a sentinel-symbol edge via the
`unresolved_names` table).

Task 3 created `crates/crabcc-core/src/resolve.rs`, which exposes:

```rust
// crates/crabcc-core/src/resolve.rs (already exists; do NOT modify).

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub i64);

/// One harvested import / use spec. The resolver owns parsing per-language;
/// downstream consumers see this normalized shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSpec {
    /// Local binding the source code uses (`Foo` in `use a::b::Foo as Foo`,
    /// `Baz` in `use a::b::Bar as Baz`, `info` in `from log import info`).
    pub local: String,
    /// Fully-qualified target the import points at. Best-effort; resolver
    /// uses it for cross-file lookup. Empty string means "same as `local`".
    pub qualified: String,
}

/// Per-file scope context handed to the `Resolver` for each use/call site.
/// Borrowed from arena state owned by the extractor; cheap to construct.
pub struct ScopeCtx<'a> {
    pub file_id: i64,
    /// Module path of the file (`crate::foo::bar` for Rust; dotted for Py).
    /// Resolvers may use this to qualify bare references.
    pub current_module: Option<&'a str>,
    pub imports: &'a [ImportSpec],
    /// Names defined in THIS file. Resolver checks this first.
    pub local_defs: &'a HashMap<String, SymbolId>,
}

pub trait Resolver: Send + Sync {
    fn resolve_ref(&self, scope: &ScopeCtx, name: &str) -> Option<SymbolId>;
    fn resolve_call(&self, scope: &ScopeCtx, callee: &str) -> Option<SymbolId>;
}

/// Cross-file symbol lookup handle. Resolvers consult this when a name
/// isn't local. Implemented by the indexer; tests pass a HashMap-backed
/// stub.
pub trait SymbolIndex: Send + Sync {
    /// Look up a symbol by its fully-qualified path (`crate::foo::Bar`).
    fn lookup_qualified(&self, qualified: &str) -> Option<SymbolId>;
    /// Look up by bare name; returns ALL candidates (ambiguity preserved).
    fn lookup_by_name(&self, name: &str) -> Vec<SymbolId>;
}
```

Task 4 split `extract.rs` into a `mod extract` directory; today the contents
live in `crates/crabcc-core/src/extract.rs` and after Task 4 the directory
layout is:

```
crates/crabcc-core/src/extract/
├── mod.rs            (the old extract.rs body, with `pub mod resolve_rust;`
│                     statements added per resolver task)
```

This task adds the Rust resolver and the `pub mod` declaration that exposes
it from `extract/mod.rs`.

## What to change

### File 1 — `crates/crabcc-core/src/extract/resolve_rust.rs` (NEW)

Create this file with exactly the following contents:

```rust
//! Rust resolver: scope-aware name resolution for Rust source files.
//!
//! At index-time we walk every `.rs` file twice. The first pass (in
//! `extract/mod.rs`) populates `local_defs` and inserts symbol rows. The
//! second pass calls into this resolver for each use-site identifier or
//! callee, which consults:
//!
//! 1. `scope.local_defs` — same-file definitions (struct, fn, trait, mod).
//! 2. `RustScope::aliases` — the per-file `use` table built from
//!    `use a::b::C [as Alias]` items. We resolve to the qualified path,
//!    then ask the cross-file `SymbolIndex` for the matching `SymbolId`.
//! 3. `<current_module>::<name>` — for bare names that match a sibling
//!    item in the same module.
//! 4. `Self::method` — looks up `method` on the enclosing `impl_target`,
//!    if any. For `Type::assoc()` we ask the index for `Type::assoc` then
//!    fall back to looking up `assoc` under the parent symbol `Type`.
//!
//! Anything we can't resolve returns `None`; the caller emits a sentinel
//! edge via `unresolved_names`. Recall is preserved, precision falls back
//! to the v3 name-only behavior for the unresolved tail.

use crate::resolve::{ImportSpec, Resolver, ScopeCtx, SymbolId, SymbolIndex};
use std::sync::Arc;
use tree_sitter::Node;

/// Per-file scope state for Rust. Built once per file by
/// [`harvest_rust_scope`] from the tree-sitter tree before the resolver
/// is invoked.
#[derive(Debug, Default, Clone)]
pub struct RustScope {
    /// `local_name -> fully_qualified_path` for every `use a::b::Local`
    /// or `use a::b::Real as Local` in the file.
    pub aliases: Vec<ImportSpec>,
    /// `mod foo { ... }` and `mod foo;` declarations in this file. Used
    /// to qualify references to `foo::Item`.
    pub child_modules: Vec<String>,
    /// `impl Type { ... }` blocks in this file, each with the methods
    /// they declare. Used for `Self::method` and `Type::method`.
    pub impl_blocks: Vec<ImplBlock>,
}

#[derive(Debug, Default, Clone)]
pub struct ImplBlock {
    /// The type being `impl`'d, with generics stripped (`Foo<T>` -> `Foo`).
    pub target: String,
    /// Method names declared inside the block.
    pub methods: Vec<String>,
    /// Byte range of the block — for "am I currently inside this impl?"
    /// checks at resolve-time.
    pub byte_start: usize,
    pub byte_end: usize,
}

/// Walk a Rust tree-sitter tree and collect [`RustScope`] data. Call once
/// per file; the result is borrowed by the resolver through `ScopeCtx`.
pub fn harvest_rust_scope(root: Node, src: &[u8]) -> RustScope {
    let mut scope = RustScope::default();
    visit(root, src, &mut scope);
    scope
}

fn visit(node: Node, src: &[u8], scope: &mut RustScope) {
    match node.kind() {
        "use_declaration" => {
            if let Some(arg) = node.child_by_field_name("argument") {
                collect_use(arg, src, "", &mut scope.aliases);
            } else {
                // Older tree-sitter-rust versions expose the path as the
                // single non-trivia child instead of a named field.
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if matches!(
                        child.kind(),
                        "scoped_identifier"
                            | "use_as_clause"
                            | "use_list"
                            | "scoped_use_list"
                            | "identifier"
                    ) {
                        collect_use(child, src, "", &mut scope.aliases);
                    }
                }
            }
        }
        "mod_item" => {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok())
            {
                scope.child_modules.push(name.to_string());
            }
        }
        "impl_item" => {
            let target = node
                .child_by_field_name("type")
                .and_then(|n| n.utf8_text(src).ok())
                .map(strip_generics)
                .map(str::to_string);
            if let Some(target) = target {
                let mut block = ImplBlock {
                    target,
                    methods: Vec::new(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                };
                if let Some(body) = node.child_by_field_name("body") {
                    collect_methods(body, src, &mut block.methods);
                }
                scope.impl_blocks.push(block);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(child, src, scope);
    }
}

fn collect_use(node: Node, src: &[u8], prefix: &str, out: &mut Vec<ImportSpec>) {
    match node.kind() {
        "identifier" | "self" | "super" | "crate" => {
            if let Ok(text) = node.utf8_text(src) {
                let qualified = join_path(prefix, text);
                out.push(ImportSpec {
                    local: text.to_string(),
                    qualified,
                });
            }
        }
        "scoped_identifier" => {
            // `a::b::C` — the last segment is the local binding; the full
            // dotted path is the qualified target.
            let full = node
                .utf8_text(src)
                .unwrap_or("")
                .split_whitespace()
                .collect::<String>();
            let local = full.rsplit("::").next().unwrap_or(&full).to_string();
            out.push(ImportSpec {
                local,
                qualified: join_path(prefix, &full),
            });
        }
        "use_as_clause" => {
            // `a::b::C as Alias` — the alias names the local binding.
            let path = node
                .child_by_field_name("path")
                .and_then(|n| n.utf8_text(src).ok())
                .unwrap_or("");
            let alias = node
                .child_by_field_name("alias")
                .and_then(|n| n.utf8_text(src).ok())
                .unwrap_or("");
            if !alias.is_empty() {
                out.push(ImportSpec {
                    local: alias.to_string(),
                    qualified: join_path(prefix, path),
                });
            }
        }
        "use_list" => {
            // `{A, B, C as Alias}` — iterate children.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_use(child, src, prefix, out);
            }
        }
        "scoped_use_list" => {
            // `a::b::{C, D}` — extract the path then recurse into the list
            // with the new prefix.
            let path = node
                .child_by_field_name("path")
                .and_then(|n| n.utf8_text(src).ok())
                .unwrap_or("");
            let new_prefix = join_path(prefix, path);
            if let Some(list) = node.child_by_field_name("list") {
                collect_use(list, src, &new_prefix, out);
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_use(child, src, prefix, out);
            }
        }
    }
}

fn collect_methods(body: Node, src: &[u8], out: &mut Vec<String>) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if matches!(child.kind(), "function_item" | "function_signature_item") {
            if let Some(name) = child
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok())
            {
                out.push(name.to_string());
            }
        }
    }
}

fn strip_generics(s: &str) -> &str {
    match s.find('<') {
        Some(i) => s[..i].trim(),
        None => s.trim(),
    }
}

fn join_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}::{name}")
    }
}

/// The Rust [`Resolver`] implementation. Holds an `Arc<dyn SymbolIndex>`
/// so the extractor can clone it cheaply across files.
pub struct RustResolver {
    pub index: Arc<dyn SymbolIndex>,
    pub scope: RustScope,
}

impl RustResolver {
    pub fn new(index: Arc<dyn SymbolIndex>, scope: RustScope) -> Self {
        Self { index, scope }
    }

    fn try_aliased(&self, name: &str) -> Option<SymbolId> {
        for alias in &self.scope.aliases {
            if alias.local == name {
                if let Some(id) = self.index.lookup_qualified(&alias.qualified) {
                    return Some(id);
                }
            }
        }
        None
    }

    fn try_module_qualified(&self, scope: &ScopeCtx, name: &str) -> Option<SymbolId> {
        let module = scope.current_module?;
        let qualified = format!("{module}::{name}");
        self.index.lookup_qualified(&qualified)
    }

    fn try_assoc(&self, type_name: &str, method: &str) -> Option<SymbolId> {
        // Preferred: ask the index for the fully-qualified `Type::method`.
        let qualified = format!("{type_name}::{method}");
        if let Some(id) = self.index.lookup_qualified(&qualified) {
            return Some(id);
        }
        // Fallback: scan our local impl blocks for the type, then look the
        // method up by name. Multiple candidates collapse to the first.
        for block in &self.scope.impl_blocks {
            if block.target == type_name && block.methods.iter().any(|m| m == method) {
                for cand in self.index.lookup_by_name(method) {
                    return Some(cand);
                }
            }
        }
        None
    }
}

impl Resolver for RustResolver {
    fn resolve_ref(&self, scope: &ScopeCtx, name: &str) -> Option<SymbolId> {
        if let Some(id) = scope.local_defs.get(name) {
            return Some(*id);
        }
        if let Some(id) = self.try_aliased(name) {
            return Some(id);
        }
        if let Some(id) = self.try_module_qualified(scope, name) {
            return Some(id);
        }
        None
    }

    fn resolve_call(&self, scope: &ScopeCtx, callee: &str) -> Option<SymbolId> {
        // `Self::method` — pick the enclosing impl by byte range. The
        // extractor passes the call-site byte offset by embedding it in
        // `callee` as `Self::method@<offset>` is NOT the contract; we
        // approximate by trying every impl block whose target matches an
        // already-resolved `Self`. Simpler: if callee is `Self::method`,
        // try every impl block in this file in source order.
        if let Some(method) = callee.strip_prefix("Self::") {
            for block in &self.scope.impl_blocks {
                if let Some(id) = self.try_assoc(&block.target, method) {
                    return Some(id);
                }
            }
            return None;
        }
        // `Type::method` — split on `::` and resolve the type first.
        if let Some((type_name, method)) = callee.split_once("::") {
            // Type may itself be aliased through `use`.
            let resolved_type = self
                .scope
                .aliases
                .iter()
                .find(|a| a.local == type_name)
                .map(|a| a.qualified.as_str())
                .unwrap_or(type_name);
            return self.try_assoc(resolved_type, method);
        }
        // Bare identifier (`foo()`): fall through to the ref path.
        self.resolve_ref(scope, callee)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tree_sitter::Parser;

    /// Cross-file index stub for the unit tests. Hand-populates a HashMap
    /// from qualified path -> SymbolId; `lookup_by_name` scans values
    /// whose qualified path ends in `::name` or equals `name`.
    #[derive(Default)]
    struct StubIndex {
        qualified: Mutex<HashMap<String, SymbolId>>,
    }

    impl StubIndex {
        fn insert(&self, qualified: &str, id: i64) {
            self.qualified
                .lock()
                .unwrap()
                .insert(qualified.to_string(), SymbolId(id));
        }
    }

    impl SymbolIndex for StubIndex {
        fn lookup_qualified(&self, qualified: &str) -> Option<SymbolId> {
            self.qualified.lock().unwrap().get(qualified).copied()
        }
        fn lookup_by_name(&self, name: &str) -> Vec<SymbolId> {
            let needle_suffix = format!("::{name}");
            self.qualified
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| k.ends_with(&needle_suffix) || k.as_str() == name)
                .map(|(_, v)| *v)
                .collect()
        }
    }

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        p.parse(src, None).unwrap()
    }

    #[test]
    fn harvests_simple_use_and_alias() {
        let src = "use a::b::Foo;\nuse a::b::Bar as Baz;\nfn main() {}\n";
        let tree = parse(src);
        let scope = harvest_rust_scope(tree.root_node(), src.as_bytes());
        let locals: Vec<&str> = scope.aliases.iter().map(|a| a.local.as_str()).collect();
        assert!(locals.contains(&"Foo"), "aliases: {:?}", scope.aliases);
        assert!(locals.contains(&"Baz"), "aliases: {:?}", scope.aliases);
        let baz = scope.aliases.iter().find(|a| a.local == "Baz").unwrap();
        assert_eq!(baz.qualified, "a::b::Bar");
    }

    #[test]
    fn harvests_use_list_and_scoped_use_list() {
        let src = "use a::b::{C, D as DD};\nfn main() {}\n";
        let tree = parse(src);
        let scope = harvest_rust_scope(tree.root_node(), src.as_bytes());
        let c = scope.aliases.iter().find(|a| a.local == "C").unwrap();
        assert_eq!(c.qualified, "a::b::C");
        let dd = scope.aliases.iter().find(|a| a.local == "DD").unwrap();
        assert_eq!(dd.qualified, "a::b::D");
    }

    #[test]
    fn harvests_impl_methods() {
        let src = "struct Foo;\nimpl Foo { fn bar(&self) {} fn baz() {} }\n";
        let tree = parse(src);
        let scope = harvest_rust_scope(tree.root_node(), src.as_bytes());
        assert_eq!(scope.impl_blocks.len(), 1);
        let block = &scope.impl_blocks[0];
        assert_eq!(block.target, "Foo");
        assert!(block.methods.contains(&"bar".to_string()));
        assert!(block.methods.contains(&"baz".to_string()));
    }

    #[test]
    fn resolves_local_def_first() {
        let src = "struct Foo;\nfn use_it(_x: Foo) {}\n";
        let tree = parse(src);
        let scope = harvest_rust_scope(tree.root_node(), src.as_bytes());
        let mut local_defs: HashMap<String, SymbolId> = HashMap::new();
        local_defs.insert("Foo".into(), SymbolId(42));
        let index = Arc::new(StubIndex::default());
        // Even with a different cross-file Foo at qualified "other::Foo",
        // local must win.
        index.insert("other::Foo", 999);
        let resolver = RustResolver::new(index, scope);
        let ctx = ScopeCtx {
            file_id: 1,
            current_module: Some("mycrate"),
            imports: &[],
            local_defs: &local_defs,
        };
        assert_eq!(resolver.resolve_ref(&ctx, "Foo"), Some(SymbolId(42)));
    }

    #[test]
    fn resolves_through_alias() {
        let src = "use other::Foo as F;\nfn x(_: F) {}\n";
        let tree = parse(src);
        let scope = harvest_rust_scope(tree.root_node(), src.as_bytes());
        let index = Arc::new(StubIndex::default());
        index.insert("other::Foo", 7);
        let resolver = RustResolver::new(index, scope);
        let local_defs: HashMap<String, SymbolId> = HashMap::new();
        let ctx = ScopeCtx {
            file_id: 1,
            current_module: Some("mycrate"),
            imports: &[],
            local_defs: &local_defs,
        };
        assert_eq!(resolver.resolve_ref(&ctx, "F"), Some(SymbolId(7)));
    }

    #[test]
    fn resolves_self_assoc_via_impl_block() {
        let src = "struct Foo;\nimpl Foo { fn bar() {} fn caller() { Self::bar(); } }\n";
        let tree = parse(src);
        let scope = harvest_rust_scope(tree.root_node(), src.as_bytes());
        let index = Arc::new(StubIndex::default());
        index.insert("Foo::bar", 11);
        let resolver = RustResolver::new(index, scope);
        let local_defs: HashMap<String, SymbolId> = HashMap::new();
        let ctx = ScopeCtx {
            file_id: 1,
            current_module: None,
            imports: &[],
            local_defs: &local_defs,
        };
        assert_eq!(resolver.resolve_call(&ctx, "Self::bar"), Some(SymbolId(11)));
    }

    #[test]
    fn unresolved_returns_none() {
        let src = "fn x() { mystery(); }\n";
        let tree = parse(src);
        let scope = harvest_rust_scope(tree.root_node(), src.as_bytes());
        let index = Arc::new(StubIndex::default());
        let resolver = RustResolver::new(index, scope);
        let local_defs: HashMap<String, SymbolId> = HashMap::new();
        let ctx = ScopeCtx {
            file_id: 1,
            current_module: Some("mycrate"),
            imports: &[],
            local_defs: &local_defs,
        };
        assert!(resolver.resolve_call(&ctx, "mystery").is_none());
    }
}
```

### File 2 — `crates/crabcc-core/src/extract/mod.rs` (edit only)

Add the single line `pub mod resolve_rust;` near the top of the file, after
the existing module-level `use` statements and before any other items. Do
not change any other line.

If multiple resolver tasks (Task 5 / 6 / 7) land in the same wave and edit
this file, each only adds its own `pub mod resolve_<lang>;` line. The
allow-list for THIS task is exactly those two files; the coder must not
touch any other location in `extract/mod.rs` or anywhere else in the tree.

Do not run `cargo build`, `cargo test`, or any other build or test command.

Do not modify any other file. Do not invent extra files.

Then commit with this exact message:

    feat(resolve): Rust scope walker — use/mod/impl-aware resolution
