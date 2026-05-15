//! Shared per-language fixtures used by both the correctness and the
//! integration tests. Kept tiny on purpose — the goal is to verify that
//! the LSP round-trips the right shapes, not to stress-test parsing.
//!
//! `#[allow(dead_code)]`: this module is included by two integration test
//! targets (`integration_lsp.rs` and `swift_extractor.rs`) via `mod
//! fixtures`. Each target sees only the constants it actually references
//! — the rest look unused to that target's linter run. The constants are
//! load-bearing in the sibling target.

#![allow(dead_code)]

pub const RUST_SRC: &str = r#"
pub struct UcraccStore {
    pub name: String,
}

impl UcraccStore {
    pub fn open(name: &str) -> Self {
        Self { name: name.to_string() }
    }

    pub fn greet(&self) {
        say_hello(&self.name);
    }
}

fn say_hello(who: &str) {
    println!("hello, {who}");
}
"#;

pub const TS_SRC: &str = r#"
export class UcraccClient {
    name: string;
    constructor(name: string) {
        this.name = name;
    }
    public greet(): void {
        sayHello(this.name);
    }
}

function sayHello(who: string): void {
    console.log("hello, " + who);
}
"#;

pub const PY_SRC: &str = r#"
class UcraccPipeline:
    def __init__(self, name: str) -> None:
        self.name = name

    def run(self) -> None:
        say_hello(self.name)

def say_hello(who: str) -> None:
    print(f"hello, {who}")
"#;

pub const SWIFT_SRC: &str = r#"
import Foundation

public class UcraccSwift {
    let name: String

    public init(name: String) {
        self.name = name
    }

    public func greet() {
        sayHello(self.name)
    }
}

func sayHello(_ who: String) {
    print("hello, \(who)")
}
"#;

pub fn all() -> &'static [(&'static str, &'static str)] {
    &[
        ("ucracc.rs", RUST_SRC),
        ("ucracc.ts", TS_SRC),
        ("ucracc.py", PY_SRC),
        ("ucracc.swift", SWIFT_SRC),
    ]
}
