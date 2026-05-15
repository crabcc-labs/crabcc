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
#[cfg(any(feature = "yaml", feature = "markdown"))]
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

#[cfg(any(feature = "yaml", feature = "markdown"))]
fn parse_only(lang: &tree_sitter::Language, src: &str) {
    let mut p = Parser::new();
    p.set_language(lang).unwrap();
    let t = p.parse(src, None).unwrap();
    black_box(t);
}

fn bench_swift(c: &mut Criterion) {
    // crabcc-core owns swift parsing as of v0.2.0 — bench through its
    // public API so the number reflects what consumers actually pay.
    c.bench_function("parse_and_walk_swift", |b| {
        b.iter(|| {
            let (s, e) = crabcc_core::extract::extract_file_with_edges(
                "u.swift",
                black_box(SWIFT_SRC),
                "swift",
            )
            .unwrap();
            black_box((s, e));
        })
    });
}

fn bench_bash(c: &mut Criterion) {
    c.bench_function("parse_and_walk_bash", |b| {
        b.iter(|| {
            let (s, e) = crabcc_core::extract::extract_file_with_edges(
                "u.sh",
                black_box(BASH_SRC),
                "bash",
            )
            .unwrap();
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

#[cfg(not(feature = "yaml"))]
fn bench_yaml(_: &mut Criterion) {}
#[cfg(not(feature = "markdown"))]
fn bench_markdown(_: &mut Criterion) {}

/// Demonstrates the tree-sitter incremental-reparse win on a single-byte
/// edit in a ~100-function Rust file. Compares:
///   * full_reparse   — parse the whole file from scratch
///   * incremental    — parse with the previously-edited Tree as input
fn bench_incremental_reparse(c: &mut Criterion) {
    // Build a ~100-fn Rust source (~3 KLOC of identifiers).
    let mut src = String::new();
    for i in 0..100 {
        src.push_str(&format!(
            "pub fn handler_{i}(input: &str) -> String {{\n    let mid = format!(\"{{input}}-{i}\");\n    helper_{i}(&mid)\n}}\n\nfn helper_{i}(s: &str) -> String {{\n    s.to_string()\n}}\n\n"
        ));
    }
    let ts_lang = crabcc_core::extract::language("rust").unwrap();

    c.bench_function("full_reparse_rust_100fn", |b| {
        b.iter(|| {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&ts_lang).unwrap();
            let t = p.parse(black_box(&src), None).unwrap();
            black_box(t);
        });
    });

    // Set up an edited prior tree (one byte appended to one identifier).
    let mut warm_parser = tree_sitter::Parser::new();
    warm_parser.set_language(&ts_lang).unwrap();
    let original_tree = warm_parser.parse(&src, None).unwrap();
    let needle = "handler_42";
    let pos = src.find(needle).unwrap();
    let mut edited_src = src.clone();
    edited_src.insert(pos + needle.len(), 'X');

    // Build the InputEdit that describes the insertion.
    let line = src[..pos].matches('\n').count();
    let col = pos - src[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let start_byte = pos + needle.len();
    let edit = tree_sitter::InputEdit {
        start_byte,
        old_end_byte: start_byte,
        new_end_byte: start_byte + 1,
        start_position: tree_sitter::Point {
            row: line,
            column: col + needle.len(),
        },
        old_end_position: tree_sitter::Point {
            row: line,
            column: col + needle.len(),
        },
        new_end_position: tree_sitter::Point {
            row: line,
            column: col + needle.len() + 1,
        },
    };

    c.bench_function("incremental_reparse_rust_100fn_one_byte_edit", |b| {
        b.iter_batched(
            // Each iteration starts from a fresh edited tree (parsing the
            // original then applying the edit) so we measure only the
            // incremental reparse cost, not the setup.
            || {
                let mut p = tree_sitter::Parser::new();
                p.set_language(&ts_lang).unwrap();
                let mut t = p.parse(&src, None).unwrap();
                t.edit(&edit);
                (p, t)
            },
            |(mut p, prior)| {
                let t = p.parse(black_box(&edited_src), Some(&prior)).unwrap();
                black_box(t);
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(50).warm_up_time(std::time::Duration::from_millis(300));
    targets = bench_swift, bench_bash, bench_yaml, bench_markdown, bench_incremental_reparse
);
criterion_main!(benches);
