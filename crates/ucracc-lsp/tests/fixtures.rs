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

pub const RUBY_SRC: &str = r#"
class UcraccRuby
  def initialize(name)
    @name = name
  end

  def greet
    say_hello(@name)
  end
end

def say_hello(who)
  puts "hello, #{who}"
end
"#;

pub const GO_SRC: &str = r#"
package ucracc

import "fmt"

type UcraccGo struct {
    Name string
}

func (u *UcraccGo) Greet() {
    sayHello(u.Name)
}

func sayHello(who string) {
    fmt.Printf("hello, %s\n", who)
}
"#;

/// A second Rust file that uses `say_hello` from `ucracc.rs` — used by
/// the cross-file `references` test to make sure we don't only return
/// same-file hits.
pub const RUST_USER_SRC: &str = r#"
fn external_user() {
    crate::say_hello("from-another-file");
}
"#;

pub const BASH_SRC: &str = r#"#!/usr/bin/env bash
UCRACC_NAME="ucracc"

ucracc_greet() {
    local who="$1"
    echo "hello, $who"
}

ucracc_main() {
    ucracc_greet "$UCRACC_NAME"
}

ucracc_main
"#;

pub const YAML_SRC: &str = r#"
name: ucracc-ci
on:
  push:
    branches: [main]
jobs:
  ucracc-build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo test -p ucracc-lsp
"#;

pub const MARKDOWN_SRC: &str = r#"# UcraccLsp

A short description.

## Installation

Run `cargo install`.

## Usage

### Quick start

Type something.

### Advanced

More things.
"#;

pub fn all() -> &'static [(&'static str, &'static str)] {
    &[
        ("ucracc.rs", RUST_SRC),
        ("ucracc.ts", TS_SRC),
        ("ucracc.py", PY_SRC),
        ("ucracc.swift", SWIFT_SRC),
        ("ucracc.rb", RUBY_SRC),
        ("ucracc.go", GO_SRC),
        ("ucracc.sh", BASH_SRC),
        ("ucracc.yaml", YAML_SRC),
        ("ucracc.md", MARKDOWN_SRC),
    ]
}
