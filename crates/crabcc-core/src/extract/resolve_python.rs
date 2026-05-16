//! Python resolver: scope-aware name resolution for Python source files.
//!
//! Two scoping facts matter for crabcc's edge model:
//!
//! 1. `import` statements — `import x.y.z`, `import x.y as z`,
//!    `from x.y import a, b as c`. Each produces one [`ImportSpec`]
//!    keyed on the local binding the source uses.
//! 2. Class bodies — `class Foo: def bar(self): self.baz()`. We track
//!    which class body each method lives in so `self.method()` and
//!    `ClassName.method()` resolve onto class-scoped members.
//!
//! Out of scope for v4.0: star imports (`from x import *`), conditional
//! imports inside functions, `__all__`-driven re-exports, and dynamic
//! `importlib` calls. Those emit unresolved-name edges via the sentinel
//! pattern.

use crate::resolve::{ImportSpec, Resolver, ScopeCtx, SymbolId, SymbolIndex};
use std::sync::Arc;
use tree_sitter::Node;

/// Per-file scope state for Python. Built once per file by
/// [`harvest_python_scope`] from the tree-sitter tree before the resolver
/// is invoked.
#[derive(Debug, Default, Clone)]
pub struct PythonScope {
    /// `local_name -> "module.path::exported"` for each import binding.
    /// We use `::` as the path separator throughout crabcc; Python's
    /// dotted source form is preserved in the module portion.
    pub aliases: Vec<ImportSpec>,
    /// Classes declared in this file, each with the methods they declare.
    pub classes: Vec<ClassBlock>,
}

#[derive(Debug, Default, Clone)]
pub struct ClassBlock {
    /// Class name (`Foo` in `class Foo(Bar): ...`).
    pub name: String,
    /// Method names defined in the body.
    pub methods: Vec<String>,
    pub byte_start: usize,
    pub byte_end: usize,
}

/// Walk a Python tree-sitter tree and collect [`PythonScope`] data.
pub fn harvest_python_scope(root: Node, src: &[u8]) -> PythonScope {
    let mut scope = PythonScope::default();
    visit(root, src, &mut scope);
    scope
}

fn visit(node: Node, src: &[u8], scope: &mut PythonScope) {
    match node.kind() {
        "import_statement" => collect_import_statement(node, src, &mut scope.aliases),
        "import_from_statement" => collect_import_from(node, src, &mut scope.aliases),
        "class_definition" => {
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

/// `import a.b.c` / `import a.b as c` / `import a, b as bb`.
fn collect_import_statement(node: Node, src: &[u8], out: &mut Vec<ImportSpec>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "dotted_name" => {
                if let Ok(path) = child.utf8_text(src) {
                    let local = path.split('.').next().unwrap_or(path).to_string();
                    out.push(ImportSpec {
                        local,
                        qualified: path.to_string(),
                    });
                }
            }
            "aliased_import" => {
                let path = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(src).ok())
                    .unwrap_or("");
                let alias = child
                    .child_by_field_name("alias")
                    .and_then(|n| n.utf8_text(src).ok())
                    .unwrap_or("");
                if !alias.is_empty() {
                    out.push(ImportSpec {
                        local: alias.to_string(),
                        qualified: path.to_string(),
                    });
                }
            }
            _ => {}
        }
    }
}

/// `from a.b import c, d as dd`.
fn collect_import_from(node: Node, src: &[u8], out: &mut Vec<ImportSpec>) {
    let module = node
        .child_by_field_name("module_name")
        .and_then(|n| n.utf8_text(src).ok())
        .unwrap_or("")
        .to_string();
    // `name` field appears once per imported binding (tree-sitter-python
    // exposes the post-`import` list as repeated `name` fields).
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "dotted_name" => {
                // Skip the leading module dotted_name; tree-sitter doesn't
                // distinguish it from the imported names by kind, so we
                // compare byte ranges to the `module_name` field above.
                if let Some(mod_node) = node.child_by_field_name("module_name") {
                    if child.byte_range() == mod_node.byte_range() {
                        continue;
                    }
                }
                if let Ok(name) = child.utf8_text(src) {
                    let local = name.split('.').next().unwrap_or(name).to_string();
                    out.push(ImportSpec {
                        local,
                        qualified: qualify(&module, name),
                    });
                }
            }
            "aliased_import" => {
                let name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(src).ok())
                    .unwrap_or("");
                let alias = child
                    .child_by_field_name("alias")
                    .and_then(|n| n.utf8_text(src).ok())
                    .unwrap_or("");
                if !alias.is_empty() && !name.is_empty() {
                    out.push(ImportSpec {
                        local: alias.to_string(),
                        qualified: qualify(&module, name),
                    });
                }
            }
            _ => {}
        }
    }
}

fn collect_class_methods(body: Node, src: &[u8], out: &mut Vec<String>) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                if let Some(name) = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(src).ok())
                {
                    out.push(name.to_string());
                }
            }
            "decorated_definition" => {
                // `@staticmethod / @classmethod` wraps the function in a
                // decorated_definition node; descend to the inner def.
                if let Some(def) = child.child_by_field_name("definition") {
                    if def.kind() == "function_definition" {
                        if let Some(name) = def
                            .child_by_field_name("name")
                            .and_then(|n| n.utf8_text(src).ok())
                        {
                            out.push(name.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn qualify(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_string()
    } else {
        format!("{module}::{name}")
    }
}

/// The Python [`Resolver`] implementation. One instance per file —
/// `scope` is the harvested per-file state, `index` is shared.
pub struct PythonResolver {
    pub index: Arc<dyn SymbolIndex>,
    pub scope: PythonScope,
}

impl PythonResolver {
    pub fn new(index: Arc<dyn SymbolIndex>, scope: PythonScope) -> Self {
        Self { index, scope }
    }

    fn try_alias(&self, name: &str) -> Option<SymbolId> {
        for alias in &self.scope.aliases {
            if alias.local == name {
                if let Some(id) = self.index.lookup_qualified(&alias.qualified) {
                    return Some(id);
                }
                let candidates = self.index.lookup_by_name(name);
                if let Some(id) = candidates.into_iter().next() {
                    return Some(id);
                }
            }
        }
        None
    }

    fn try_class_method(&self, class_name: &str, method: &str) -> Option<SymbolId> {
        let qualified = format!("{class_name}::{method}");
        if let Some(id) = self.index.lookup_qualified(&qualified) {
            return Some(id);
        }
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

impl Resolver for PythonResolver {
    fn resolve_ref(&self, scope: &ScopeCtx, name: &str) -> Option<SymbolId> {
        if let Some(id) = scope.local_defs.get(name) {
            return Some(*id);
        }
        if let Some(id) = self.try_alias(name) {
            return Some(id);
        }
        let candidates = self.index.lookup_by_name(name);
        if candidates.len() == 1 {
            return Some(candidates[0]);
        }
        None
    }

    fn resolve_call(&self, scope: &ScopeCtx, callee: &str) -> Option<SymbolId> {
        // `self.method` — try every class block in source order.
        if let Some(method) = callee.strip_prefix("self.") {
            for class in &self.scope.classes {
                if class.methods.iter().any(|m| m == method) {
                    if let Some(id) = self.try_class_method(&class.name, method) {
                        return Some(id);
                    }
                }
            }
            return None;
        }
        // `ClassName.method` — split on the first `.`, resolve the class
        // through aliases, then look the method up.
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

    fn parse_py(src: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        p.parse(src, None).unwrap()
    }

    #[test]
    fn harvests_import_dotted_and_aliased() {
        let src = "import a.b.c\nimport a.b as ab\n";
        let tree = parse_py(src);
        let scope = harvest_python_scope(tree.root_node(), src.as_bytes());
        let locals: Vec<&str> = scope.aliases.iter().map(|a| a.local.as_str()).collect();
        assert!(locals.contains(&"a"), "aliases: {:?}", scope.aliases);
        assert!(locals.contains(&"ab"), "aliases: {:?}", scope.aliases);
        let ab = scope.aliases.iter().find(|a| a.local == "ab").unwrap();
        assert_eq!(ab.qualified, "a.b");
    }

    #[test]
    fn harvests_from_import_with_alias() {
        let src = "from x.y import foo, bar as bz\n";
        let tree = parse_py(src);
        let scope = harvest_python_scope(tree.root_node(), src.as_bytes());
        let foo = scope.aliases.iter().find(|a| a.local == "foo").unwrap();
        assert_eq!(foo.qualified, "x.y::foo");
        let bz = scope.aliases.iter().find(|a| a.local == "bz").unwrap();
        assert_eq!(bz.qualified, "x.y::bar");
    }

    #[test]
    fn harvests_class_with_methods_and_decorators() {
        let src = "class Foo:\n    def bar(self): pass\n    @staticmethod\n    def baz(): pass\n";
        let tree = parse_py(src);
        let scope = harvest_python_scope(tree.root_node(), src.as_bytes());
        assert_eq!(scope.classes.len(), 1);
        let cls = &scope.classes[0];
        assert_eq!(cls.name, "Foo");
        assert!(cls.methods.contains(&"bar".to_string()));
        assert!(cls.methods.contains(&"baz".to_string()));
    }

    #[test]
    fn resolves_local_def_first() {
        let src = "class Foo: pass\n";
        let tree = parse_py(src);
        let scope = harvest_python_scope(tree.root_node(), src.as_bytes());
        let mut local_defs: HashMap<String, SymbolId> = HashMap::new();
        local_defs.insert("Foo".into(), SymbolId(42));
        let index = Arc::new(StubIndex::default());
        index.insert("other::Foo", 999);
        let resolver = PythonResolver::new(index, scope);
        let ctx = ScopeCtx {
            file_id: 1,
            current_module: Some("m"),
            imports: &[],
            local_defs: &local_defs,
        };
        assert_eq!(resolver.resolve_ref(&ctx, "Foo"), Some(SymbolId(42)));
    }

    #[test]
    fn resolves_through_from_import_alias() {
        let src = "from other import Bar as B\nx: B = None\n";
        let tree = parse_py(src);
        let scope = harvest_python_scope(tree.root_node(), src.as_bytes());
        let index = Arc::new(StubIndex::default());
        index.insert("other::Bar", 7);
        let resolver = PythonResolver::new(index, scope);
        let local_defs: HashMap<String, SymbolId> = HashMap::new();
        let ctx = ScopeCtx {
            file_id: 1,
            current_module: Some("m"),
            imports: &[],
            local_defs: &local_defs,
        };
        assert_eq!(resolver.resolve_ref(&ctx, "B"), Some(SymbolId(7)));
    }

    #[test]
    fn resolves_self_method_via_class_block() {
        let src = "class Foo:\n    def bar(self): self.baz()\n    def baz(self): pass\n";
        let tree = parse_py(src);
        let scope = harvest_python_scope(tree.root_node(), src.as_bytes());
        let index = Arc::new(StubIndex::default());
        index.insert("Foo::baz", 11);
        let resolver = PythonResolver::new(index, scope);
        let local_defs: HashMap<String, SymbolId> = HashMap::new();
        let ctx = ScopeCtx {
            file_id: 1,
            current_module: None,
            imports: &[],
            local_defs: &local_defs,
        };
        assert_eq!(resolver.resolve_call(&ctx, "self.baz"), Some(SymbolId(11)));
    }

    #[test]
    fn resolves_classname_dot_method() {
        let src = "class Foo:\n    def go(self): pass\n";
        let tree = parse_py(src);
        let scope = harvest_python_scope(tree.root_node(), src.as_bytes());
        let index = Arc::new(StubIndex::default());
        index.insert("Foo::go", 13);
        let resolver = PythonResolver::new(index, scope);
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
        let src = "def x(): mystery()\n";
        let tree = parse_py(src);
        let scope = harvest_python_scope(tree.root_node(), src.as_bytes());
        let index = Arc::new(StubIndex::default());
        let resolver = PythonResolver::new(index, scope);
        let local_defs: HashMap<String, SymbolId> = HashMap::new();
        let ctx = ScopeCtx {
            file_id: 1,
            current_module: Some("m"),
            imports: &[],
            local_defs: &local_defs,
        };
        assert!(resolver.resolve_call(&ctx, "mystery").is_none());
    }
}
