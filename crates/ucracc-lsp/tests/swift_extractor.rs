//! Correctness test for the Swift extractor. Parses a realistic Swift
//! source and asserts the extracted symbols and edges match what the
//! navigation surface depends on.
//!
//! Run: cargo test -p ucracc-lsp --test swift_extractor

#![cfg(feature = "swift")]

mod fixtures;

use crabcc_core::SymbolKind;

#[test]
fn swift_class_func_call_edge_extraction() {
    let (syms, edges) =
        ucracc_lsp::swift::extract("ucracc.swift", fixtures::SWIFT_SRC)
            .expect("swift extraction must succeed");

    // We must capture: the public class, its init, its greet method, the
    // free `sayHello` function.
    let by_name = |n: &str| syms.iter().find(|s| s.name == n);

    let class = by_name("UcraccSwift").expect("class UcraccSwift missing");
    assert_eq!(class.kind, SymbolKind::Class);
    assert_eq!(class.visibility.as_deref(), Some("public"));

    let init = by_name("UcraccSwift")
        .map(|_| ())
        .and(syms.iter().find(|s| s.kind == SymbolKind::Method));
    assert!(init.is_some(), "expected at least one Method (init)");

    let greet = syms
        .iter()
        .find(|s| s.kind == SymbolKind::Function && s.name == "greet");
    assert!(greet.is_some(), "expected Function `greet`");
    assert_eq!(greet.unwrap().parent.as_deref(), Some("UcraccSwift"));

    let say = by_name("sayHello").expect("free fn `sayHello` missing");
    assert_eq!(say.kind, SymbolKind::Function);
    assert!(say.parent.is_none(), "free fn should have no parent");

    // Call edges: greet() must call sayHello().
    let has_call = edges
        .iter()
        .any(|e| e.dst_name == "sayHello" && e.kind == "call");
    assert!(has_call, "missing call edge greet -> sayHello; got {edges:?}");

    // Lines must be 1-based and ordered.
    assert!(class.line_start > 0);
    assert!(class.line_end >= class.line_start);
}
