use crate::store::Store;
use crate::types::Symbol;
use anyhow::Result;

/// All symbols in `file`, ordered by line. Caller can filter to top-level
/// (parent IS NULL) or group by parent for a class/module hierarchy.
pub fn outline(store: &Store, file: &str) -> Result<Vec<Symbol>> {
    store.symbols_in_file(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::build_index;
    use std::path::Path;

    fn write(p: &Path, body: &str) {
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn outline_typescript_class_orders_by_line() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("greet.ts"), r#"
export function alpha(){return 1}
export class Greeter {
  greet(n: string){ return n }
  bye(){ return "bye" }
}
export function omega(){return 2}
"#);
        let store = Store::open(&root.join("idx.db")).unwrap();
        build_index(root, &store).unwrap();
        let syms = outline(&store, "greet.ts").unwrap();

        // Must be ordered by line.
        let lines: Vec<u32> = syms.iter().map(|s| s.line_start).collect();
        let mut sorted = lines.clone();
        sorted.sort();
        assert_eq!(lines, sorted, "outline must be line-sorted");

        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"greet"));
        assert!(names.contains(&"omega"));
    }

    #[test]
    fn outline_unknown_file_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("idx.db")).unwrap();
        let syms = outline(&store, "nope.ts").unwrap();
        assert_eq!(syms.len(), 0);
    }
}
