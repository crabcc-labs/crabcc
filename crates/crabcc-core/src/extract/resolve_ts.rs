//! TypeScript / JavaScript resolver: scope-aware name resolution.
//!
//! The shape of TS and JS scoping that matters for crabcc edges is small:
//!
//! 1. ES module imports — `import { foo, bar as baz } from './path'`,
//!    `import foo from './path'`, `import * as ns from './path'`. We
//!    record one [`ImportSpec`] per local binding, with `qualified` set
//!    to `<module>::<exported_name>` (using `::` to match the rest of
//!    crabcc's qualifier convention).
//! 2. Class scope — `class Foo { method() { this.other(); } }`. We track
//!    which class body we're inside so `this.method()` and
//!    `Foo.staticMethod()` resolve onto class-scoped members instead of
//!    bare names.
//!
//! Anything we can't resolve returns `None`; the extractor emits a
//! sentinel edge via `unresolved_names`. Re-exports, dynamic imports
//! (`import('x')`), and CommonJS `require` are intentionally
//! out-of-scope for v4.0 — they emit unresolved-name edges.

use crate::resolve::{ImportSpec, Resolver, ScopeCtx, SymbolId, SymbolIndex};
use std::sync::Arc;
use tree_sitter::Node;

/// Per-file scope state for TS/JS. Built once per file by
/// [`harvest_ts_scope`] from the tree-sitter tree before the resolver
/// is invoked. Works for both `tree_sitter_typescript` and
/// `tree_sitter_javascript` — the relevant node kinds overlap.
#[derive(Debug, Default, Clone)]
pub struct TsScope {
    /// `local_name -> \"<module>::<exported>\"` for each import binding.
    pub aliases: Vec<ImportSpec>,
    /// Classes declared in this file, each with the methods they declare.
    pub classes: Vec<ClassBlock>,
}

#[derive(Debug, Default, Clone)]
pub struct ClassBlock {
    /// Class name (`Foo` in `class Foo extends Bar { ... }`).
    pub name: String,
    /// Method names declared inside the body — both instance and static.
    pub methods: Vec<String>,
    pub byte_start: usize,
    pub byte_end: usize,
}

/// Walk a TS/JS tree-sitter tree and collect [`TsScope`] data.
pub fn harvest_ts_scope(root: Node, src: &[u8]) -> TsScope {
    let mut scope = TsScope::default();
    visit(root, src, &mut scope);
    scope
}

fn visit(node: Node, src: &[u8], scope: &mut TsScope) {
    match node.kind() {
        "import_statement" => collect_import(node, src, &mut scope.aliases),
        "class_declaration" => {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok())
            {
                let mut block = ClassBlock {
                    name: name.to_string(),
                    methods: Vec::new(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                };
                if let Some(body) = node.child_by_field_name("body") {
                    collect_class_methods(body, src, &mut block.methods);
                }
                scope.classes.push(block);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(child, src, scope);
    }
}

fn collect_import(node: Node, src: &[u8], out: &mut Vec<ImportSpec>) {
    // The module specifier sits on the `source` field as a string node.
    let module = node
        .child_by_field_name("source")
        .and_then(|n| n.utf8_text(src).ok())
        .map(strip_quotes)
        .unwrap_or_default()
        .to_string();
    // The import clause shape varies — walk children of the import
    // statement looking for the named/default/namespace forms.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "import_clause" => collect_import_clause(child, src, &module, out),
            // Some grammar versions expose `named_imports` / `identifier`
            // directly under `import_statement`.
            "named_imports" => collect_named_imports(child, src, &module, out),
            "identifier" => {
                if let Ok(name) = child.utf8_text(src) {
                    out.push(ImportSpec {
                        local: name.to_string(),
                        qualified: qualify(&module, name),
                    });
                }
            }
            "namespace_import" => {
                if let Some(alias) = namespace_alias(child, src) {
                    out.push(ImportSpec {
                        local: alias,
                        qualified: module.clone(),
                    });
                }
            }
            _ => {}
        }
    }
}

fn collect_import_clause(node: Node, src: &[u8], module: &str, out: &mut Vec<ImportSpec>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            // `import foo from './x'`
            "identifier" => {
                if let Ok(name) = child.utf8_text(src) {
                    out.push(ImportSpec {
                        local: name.to_string(),
                        qualified: qualify(module, "default"),
                    });
                }
            }
            // `import { a, b as c } from './x'`
            "named_imports" => collect_named_imports(child, src, module, out),
            // `import * as ns from './x'`
            "namespace_import" => {
                if let Some(alias) = namespace_alias(child, src) {
                    out.push(ImportSpec {
                        local: alias,
                        qualified: module.to_string(),
                    });
                }
            }
            _ => {}
        }
    }
}

fn collect_named_imports(node: Node, src: &[u8], module: &str, out: &mut Vec<ImportSpec>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "import_specifier" {
            let name = child
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok());
            let alias = child
                .child_by_field_name("alias")
                .and_then(|n| n.utf8_text(src).ok());
            if let Some(name) = name {
                let local = alias.unwrap_or(name).to_string();
                out.push(ImportSpec {
                    local,
                    qualified: qualify(module, name),
                });
            }
        }
    }
}

fn namespace_alias(node: Node, src: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return child.utf8_text(src).ok().map(str::to_string);
        }
    }
    None
}

fn collect_class_methods(body: Node, src: &[u8], out: &mut Vec<String>) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if matches!(
            child.kind(),
            "method_definition" | "method_signature" | "abstract_method_signature"
        ) {
            if let Some(name) = child
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok())
            {
                out.push(name.to_string());
            }
        }
    }
}

fn strip_quotes(s: &str) -> &str {
    s.trim_matches(|c| c == '"' || c == '\'' || c == '`')
}

fn qualify(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_string()
    } else {
        format!("{module}::{name}")
    }
}

/// The TS/JS [`Resolver`] implementation. One instance per file —
/// `scope` is the harvested per-file state, `index` is shared.
pub struct TsResolver {
    pub index: Arc<dyn SymbolIndex>,
    pub scope: TsScope,
}

impl TsResolver {
    pub fn new(index: Arc<dyn SymbolIndex>, scope: TsScope) -> Self {
        Self { index, scope }
    }

    fn try_alias(&self, name: &str) -> Option<SymbolId> {
        for alias in &self.scope.aliases {
            if alias.local == name {
                if let Some(id) = self.index.lookup_qualified(&alias.qualified) {
                    return Some(id);
                }
                // Fall back to bare-name lookup if the qualified form
                // didn't land (e.g. relative path didn't survive the
                // index's path normalization).
                let candidates = self.index.lookup_by_name(name);
                if let Some(id) = candidates.into_iter().next() {
                    return Some(id);
                }
            }
        }
        None
    }

    fn try_class_method(&self, class_name: &str, method: &str) -> Option<SymbolId> {
        // Preferred: ask the index for the fully-qualified `Class.method`.
        // We use `::` as crabcc's universal separator.
        let qualified = format!("{class_name}::{method}");
        if let Some(id) = self.index.lookup_qualified(&qualified) {
            return Some(id);
        }
        // Fallback: if THIS file declares `class_name`, scan its methods.
        for class in &self.scope.classes {
            if class.name == class_name && class.methods.iter().any(|m| m == method) {
                if let Some(id) = self.index.lookup_by_name(method).into_iter().next() {
                    return Some(id);
                }
            }
        }
        None
    }
}

impl Resolver for TsResolver {
    fn resolve_ref(&self, scope: &ScopeCtx, name: &str) -> Option<SymbolId> {
        if let Some(id) = scope.local_defs.get(name) {
            return Some(*id);
        }
        if let Some(id) = self.try_alias(name) {
            return Some(id);
        }
        // Bare name fallback — single cross-file candidate by name.
        let candidates = self.index.lookup_by_name(name);
        if candidates.len() == 1 {
            return Some(candidates[0]);
        }
        None
    }

    fn resolve_call(&self, scope: &ScopeCtx, callee: &str) -> Option<SymbolId> {
        // `this.method` — try every class block in source order. The
        // extractor narrows this further with byte ranges, but here we
        // pick the first matching method.
        if let Some(method) = callee.strip_prefix("this.") {
            for class in &self.scope.classes {
                if class.methods.iter().any(|m| m == method) {
                    if let Some(id) = self.try_class_method(&class.name, method) {
                        return Some(id);
                    }
                }
            }
            return None;
        }
        // `Class.method` — split, resolve class through aliases, then
        // look up the method.
        if let Some((class_name, method)) = callee.split_once('.') {
            let resolved_class = self
                .scope
                .aliases
                .iter()
                .find(|a| a.local == class_name)
                .map(|a| a.qualified.as_str())
                .unwrap_or(class_name);
            return self.try_class_method(resolved_class, method);
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
            let suffix = format!("::{name}");
            self.qualified
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| k.ends_with(&suffix) || k.as_str() == name)
                .map(|(_, v)| *v)
                .collect()
        }
    }

    fn parse_ts(src: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        p.parse(src, None).unwrap()
    }

    #[test]
    fn harvests_named_and_default_imports() {
        let src = "import foo from './m';\nimport { a, b as c } from './n';\n";
        let tree = parse_ts(src);
        let scope = harvest_ts_scope(tree.root_node(), src.as_bytes());
        let locals: Vec<&str> = scope.aliases.iter().map(|a| a.local.as_str()).collect();
        assert!(locals.contains(&"foo"), "aliases: {:?}", scope.aliases);
        assert!(locals.contains(&"a"), "aliases: {:?}", scope.aliases);
        assert!(locals.contains(&"c"), "aliases: {:?}", scope.aliases);
        let c = scope.aliases.iter().find(|a| a.local == "c").unwrap();
        assert_eq!(c.qualified, "./n::b");
        let foo = scope.aliases.iter().find(|a| a.local == "foo").unwrap();
        assert_eq!(foo.qualified, "./m::default");
    }

    #[test]
    fn harvests_namespace_import() {
        let src = "import * as utils from './u';\n";
        let tree = parse_ts(src);
        let scope = harvest_ts_scope(tree.root_node(), src.as_bytes());
        assert_eq!(scope.aliases.len(), 1);
        assert_eq!(scope.aliases[0].local, "utils");
        assert_eq!(scope.aliases[0].qualified, "./u");
    }

    #[test]
    fn harvests_class_methods() {
        let src = "class Foo { bar() {} static baz() {} }\n";
        let tree = parse_ts(src);
        let scope = harvest_ts_scope(tree.root_node(), src.as_bytes());
        assert_eq!(scope.classes.len(), 1);
        let cls = &scope.classes[0];
        assert_eq!(cls.name, "Foo");
        assert!(cls.methods.contains(&"bar".to_string()));
        assert!(cls.methods.contains(&"baz".to_string()));
    }

    #[test]
    fn resolves_local_def_first() {
        let src = "class Foo {}\n";
        let tree = parse_ts(src);
        let scope = harvest_ts_scope(tree.root_node(), src.as_bytes());
        let mut local_defs: HashMap<String, SymbolId> = HashMap::new();
        local_defs.insert("Foo".into(), SymbolId(42));
        let index = Arc::new(StubIndex::default());
        index.insert("./other::Foo", 999);
        let resolver = TsResolver::new(index, scope);
        let ctx = ScopeCtx {
            file_id: 1,
            current_module: Some("./m"),
            imports: &[],
            local_defs: &local_defs,
        };
        assert_eq!(resolver.resolve_ref(&ctx, "Foo"), Some(SymbolId(42)));
    }

    #[test]
    fn resolves_through_named_import_alias() {
        let src = "import { Bar as B } from './other';\nfunction x(b: B) {}\n";
        let tree = parse_ts(src);
        let scope = harvest_ts_scope(tree.root_node(), src.as_bytes());
        let index = Arc::new(StubIndex::default());
        index.insert("./other::Bar", 7);
        let resolver = TsResolver::new(index, scope);
        let local_defs: HashMap<String, SymbolId> = HashMap::new();
        let ctx = ScopeCtx {
            file_id: 1,
            current_module: Some("./m"),
            imports: &[],
            local_defs: &local_defs,
        };
        assert_eq!(resolver.resolve_ref(&ctx, "B"), Some(SymbolId(7)));
    }

    #[test]
    fn resolves_class_dot_static_method() {
        let src = "class Foo { static go() {} }\n";
        let tree = parse_ts(src);
        let scope = harvest_ts_scope(tree.root_node(), src.as_bytes());
        let index = Arc::new(StubIndex::default());
        index.insert("Foo::go", 13);
        let resolver = TsResolver::new(index, scope);
        let local_defs: HashMap<String, SymbolId> = HashMap::new();
        let ctx = ScopeCtx {
            file_id: 1,
            current_module: None,
            imports: &[],
            local_defs: &local_defs,
        };
        assert_eq!(resolver.resolve_call(&ctx, "Foo.go"), Some(SymbolId(13)));
    }

    #[test]
    fn unresolved_returns_none() {
        let src = "function x() { mystery(); }\n";
        let tree = parse_ts(src);
        let scope = harvest_ts_scope(tree.root_node(), src.as_bytes());
        let index = Arc::new(StubIndex::default());
        let resolver = TsResolver::new(index, scope);
        let local_defs: HashMap<String, SymbolId> = HashMap::new();
        let ctx = ScopeCtx {
            file_id: 1,
            current_module: Some("./m"),
            imports: &[],
            local_defs: &local_defs,
        };
        assert!(resolver.resolve_call(&ctx, "mystery").is_none());
    }
}