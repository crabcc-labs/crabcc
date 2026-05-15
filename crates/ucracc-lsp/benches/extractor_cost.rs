//! Where does the time go inside each per-language extractor?
//!
//! For Swift / Bash / YAML / Markdown — the four languages we extract
//! ourselves — measure:
//!   * `parse_only_*`    — tree-sitter `Parser::parse` and discard
//!   * `parse_and_walk_*` — full `extract(file, src)` including symbol/edge
//!     emission
//!
//! The delta is the walker cost (which is what would move if we delegated
//! extraction to `crabcc-core`). If parse dominates, moving the walker
//! doesn't measurably change latency — the decision is architectural,
//! not performance-driven.
//!
//! Run:
//!   cargo bench -p ucracc-lsp --bench extractor_cost

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use tree_sitter::Parser;

const SWIFT_SRC: &str = r#"
import Foundation
public class UcraccSwift {
    let name: String
    public init(name: String) { self.name = name }
    public func greet() { sayHello(self.name) }
}
func sayHello(_ who: String) { print("hello, \(who)") }
"#;

const BASH_SRC: &str = r#"#!/usr/bin/env bash
UCRACC_NAME="ucracc"
ucracc_greet() {
    local who="$1"
    echo "hello, $who"
}
ucracc_main() { ucracc_greet "$UCRACC_NAME"; }
ucracc_main
"#;

const YAML_SRC: &str = r#"
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

const MARKDOWN_SRC: &str = r#"# UcraccLsp
A short description.
## Installation
Run `cargo install`.
## Usage
### Quick start
Type something.
### Advanced
More.
"#;

fn parse_only(lang: &tree_sitter::Language, src: &str) {
    let mut p = Parser::new();
    p.set_language(lang).unwrap();
    let t = p.parse(src, None).unwrap();
    black_box(t);
}

#[cfg(feature = "swift")]
fn bench_swift(c: &mut Criterion) {
    let lang = tree_sitter_swift::LANGUAGE.into();
    c.bench_function("parse_only_swift", |b| {
        b.iter(|| parse_only(&lang, black_box(SWIFT_SRC)))
    });
    c.bench_function("parse_and_walk_swift", |b| {
        b.iter(|| {
            let (s, e) = ucracc_lsp::swift::extract("u.swift", black_box(SWIFT_SRC)).unwrap();
            black_box((s, e));
        })
    });
}

#[cfg(feature = "bash")]
fn bench_bash(c: &mut Criterion) {
    let lang = tree_sitter_bash::LANGUAGE.into();
    c.bench_function("parse_only_bash", |b| {
        b.iter(|| parse_only(&lang, black_box(BASH_SRC)))
    });
    c.bench_function("parse_and_walk_bash", |b| {
        b.iter(|| {
            let (s, e) = ucracc_lsp::bash::extract("u.sh", black_box(BASH_SRC)).unwrap();
            black_box((s, e));
        })
    });
}

#[cfg(feature = "yaml")]
fn bench_yaml(c: &mut Criterion) {
    let lang = tree_sitter_yaml::LANGUAGE.into();
    c.bench_function("parse_only_yaml", |b| {
        b.iter(|| parse_only(&lang, black_box(YAML_SRC)))
    });
    c.bench_function("parse_and_walk_yaml", |b| {
        b.iter(|| {
            let (s, e) = ucracc_lsp::yaml::extract("u.yaml", black_box(YAML_SRC)).unwrap();
            black_box((s, e));
        })
    });
}

#[cfg(feature = "markdown")]
fn bench_markdown(c: &mut Criterion) {
    let lang = tree_sitter_md::LANGUAGE.into();
    c.bench_function("parse_only_markdown", |b| {
        b.iter(|| parse_only(&lang, black_box(MARKDOWN_SRC)))
    });
    c.bench_function("parse_and_walk_markdown", |b| {
        b.iter(|| {
            let (s, e) = ucracc_lsp::markdown::extract("u.md", black_box(MARKDOWN_SRC)).unwrap();
            black_box((s, e));
        })
    });
}

#[cfg(not(feature = "swift"))]
fn bench_swift(_: &mut Criterion) {}
#[cfg(not(feature = "bash"))]
fn bench_bash(_: &mut Criterion) {}
#[cfg(not(feature = "yaml"))]
fn bench_yaml(_: &mut Criterion) {}
#[cfg(not(feature = "markdown"))]
fn bench_markdown(_: &mut Criterion) {}

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(50).warm_up_time(std::time::Duration::from_millis(300));
    targets = bench_swift, bench_bash, bench_yaml, bench_markdown
);
criterion_main!(benches);
