//! Post-mutation validation primitives.
//!
//! When an agent writes (or patches) a file, the MCP layer calls
//! [`reindex_file`] to refresh the symbol/edge tables for that path,
//! then [`diff_symbols`] to compare the post-write snapshot against
//! the pre-write snapshot captured before the disk write. The result
//! is a `SymbolDiff` describing structural blast radius — what got
//! added, removed, signature-changed, or moved.
//!
//! Tree-sitter-only: no rustc, no tsc, no language servers. Compiler
//! diagnostics are a separate optional layer.

use crate::extract;
use crate::hash;
use crate::store::Store;
use crate::types::Symbol;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum SymbolChange {
    Added { name: String, line_start: u32 },
    Removed { name: String, line_start: u32 },
    SignatureChanged { name: String, before: Option<String>, after: Option<String> },
    BodyMoved { name: String, before_line: u32, after_line: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SymbolDiff {
    pub file: String,
    pub changes: Vec<SymbolChange>,
}

impl SymbolDiff {
    pub fn is_empty(&self) -> bool { self.changes.is_empty() }
    pub fn removed_names(&self) -> Vec<String> {
        self.changes.iter().filter_map(|c| match c {
            SymbolChange::Removed { name, .. } => Some(name.clone()),
            _ => None,
        }).collect()
    }
    pub fn added_names(&self) -> Vec<String> {
        self.changes.iter().filter_map(|c| match c {
            SymbolChange::Added { name, .. } => Some(name.clone()),
            _ => None,
        }).collect()
    }
}

/// Diff two symbol snapshots for the same file. Match by `name`.
/// Order: removed → added → signature_changed → body_moved (stable).
pub fn diff_symbols(file: &str, before: &[Symbol], after: &[Symbol]) -> SymbolDiff {
    let before_map: BTreeMap<&str, &Symbol> = before.iter().map(|s| (s.name.as_str(), s)).collect();
    let after_map: BTreeMap<&str, &Symbol> = after.iter().map(|s| (s.name.as_str(), s)).collect();

    let mut removed = Vec::new();
    let mut added = Vec::new();
    let mut sig_changed = Vec::new();
    let mut body_moved = Vec::new();

    for (name, b) in &before_map {
        match after_map.get(name) {
            None => removed.push(SymbolChange::Removed {
                name: (*name).to_string(),
                line_start: b.line_start,
            }),
            Some(a) => {
                if a.signature != b.signature {
                    sig_changed.push(SymbolChange::SignatureChanged {
                        name: (*name).to_string(),
                        before: b.signature.clone(),
                        after: a.signature.clone(),
                    });
                } else if a.line_start != b.line_start {
                    body_moved.push(SymbolChange::BodyMoved {
                        name: (*name).to_string(),
                        before_line: b.line_start,
                        after_line: a.line_start,
                    });
                }
            }
        }
    }
    for (name, a) in &after_map {
        if !before_map.contains_key(name) {
            added.push(SymbolChange::Added {
                name: (*name).to_string(),
                line_start: a.line_start,
            });
        }
    }

    let mut changes = Vec::with_capacity(removed.len() + added.len() + sig_changed.len() + body_moved.len());
    changes.extend(removed);
    changes.extend(added);
    changes.extend(sig_changed);
    changes.extend(body_moved);

    SymbolDiff { file: file.to_string(), changes }
}

/// Re-extract symbols + edges from in-memory `src` and write through
/// to `store`. Returns the new symbol vector + an optional parse-error.
/// No-op (returns empty + None) if the path's extension isn't in
/// `extract::detect_lang`.
pub fn reindex_file(
    store: &Store,
    rel: &str,
    src: &str,
) -> Result<(Vec<Symbol>, Option<String>)> {
    let lang = match extract::detect_lang(Path::new(rel)) {
        Some(l) => l,
        None => return Ok((Vec::new(), None)),
    };
    let (syms, edges) = match extract::extract_file_with_edges(rel, src, lang) {
        Ok(p) => p,
        Err(e) => return Ok((Vec::new(), Some(format!("parse: {e}")))),
    };
    let sha = hash::sha256_hex(src.as_bytes());
    let mtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let fid = store.upsert_file(rel, &sha, mtime, lang)?;
    store.replace_symbols(fid, &syms)?;
    store.replace_edges(fid, &edges)?;
    Ok((syms, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SymbolKind;

    fn sym(name: &str, sig: Option<&str>, line: u32) -> Symbol {
        Symbol {
            name: name.into(),
            kind: SymbolKind::Function,
            signature: sig.map(str::to_string),
            parent: None,
            file: "x.rs".into(),
            line_start: line,
            line_end: line + 5,
            visibility: None,
        }
    }

    #[test]
    fn empty_diff_when_identical() {
        let a = vec![sym("foo", Some("fn foo()"), 10)];
        assert!(diff_symbols("x.rs", &a, &a.clone()).is_empty());
    }

    #[test]
    fn detects_added_and_removed() {
        let before = vec![sym("foo", Some("fn foo()"), 10), sym("bar", None, 30)];
        let after = vec![sym("foo", Some("fn foo()"), 10), sym("baz", None, 40)];
        let d = diff_symbols("x.rs", &before, &after);
        assert_eq!(d.removed_names(), vec!["bar".to_string()]);
        assert_eq!(d.added_names(), vec!["baz".to_string()]);
    }

    #[test]
    fn detects_signature_change() {
        let before = vec![sym("foo", Some("fn foo()"), 10)];
        let after = vec![sym("foo", Some("fn foo(x: u32)"), 10)];
        let d = diff_symbols("x.rs", &before, &after);
        assert_eq!(d.changes.len(), 1);
        assert!(matches!(d.changes[0], SymbolChange::SignatureChanged { .. }));
    }

    #[test]
    fn detects_body_moved() {
        let before = vec![sym("foo", Some("fn foo()"), 10)];
        let after = vec![sym("foo", Some("fn foo()"), 20)];
        let d = diff_symbols("x.rs", &before, &after);
        assert!(matches!(d.changes[0], SymbolChange::BodyMoved { .. }));
    }

    #[test]
    fn change_order_removed_then_added() {
        let before = vec![sym("aaa", None, 10), sym("zzz", None, 20)];
        let after = vec![sym("aaa", None, 10), sym("new", None, 30)];
        let d = diff_symbols("x.rs", &before, &after);
        assert!(matches!(d.changes[0], SymbolChange::Removed { .. }));
        assert!(matches!(d.changes[1], SymbolChange::Added { .. }));
    }
}
