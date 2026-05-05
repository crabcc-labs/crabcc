//! Microbench for the indexer's per-file tree-sitter pipeline.
//!
//! The pre-pool path called `Parser::new()` + `set_language(...)` on
//! every file; the post-pool path reuses a thread-local `Parser` per
//! language. The 1000-call benches below show the steady-state win;
//! the single-call bench measures the warm path (pool already
//! populated for the worker thread).
//!
//! Run via `cargo bench -p crabcc-core --bench extract --features bench`.

use std::hint::black_box;
use std::time::Duration;

use crabcc_core::extract::extract_file_with_edges;
use criterion::{criterion_group, criterion_main, Criterion};

const RUST_FIXTURE: &str = r#"
use std::sync::Arc;

pub struct Greeter {
    name: String,
}

impl Greeter {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub fn greet(&self, suffix: &str) -> String {
        let greeting = format!("Hello, {}{suffix}", self.name);
        emit(&greeting);
        greeting
    }
}

fn emit(s: &str) {
    println!("{s}");
}

pub fn run() {
    let g = Greeter::new("world");
    g.greet("!");
}
"#;

const PYTHON_FIXTURE: &str = r#"
class Greeter:
    def __init__(self, name: str) -> None:
        self.name = name

    def greet(self, suffix: str) -> str:
        message = f"Hello, {self.name}{suffix}"
        emit(message)
        return message


def emit(s: str) -> None:
    print(s)


def run() -> None:
    g = Greeter("world")
    g.greet("!")
"#;

const TYPESCRIPT_FIXTURE: &str = r#"
export class Greeter {
    constructor(private name: string) {}

    greet(suffix: string): string {
        const message = `Hello, ${this.name}${suffix}`;
        emit(message);
        return message;
    }
}

function emit(s: string): void {
    console.log(s);
}

export function run(): void {
    const g = new Greeter("world");
    g.greet("!");
}
"#;

fn bench_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_single");
    group.measurement_time(Duration::from_secs(3));

    group.bench_function("rust", |b| {
        b.iter(|| {
            let (s, e) = extract_file_with_edges("a.rs", black_box(RUST_FIXTURE), "rust").unwrap();
            black_box((s, e));
        });
    });
    group.bench_function("python", |b| {
        b.iter(|| {
            let (s, e) =
                extract_file_with_edges("a.py", black_box(PYTHON_FIXTURE), "python").unwrap();
            black_box((s, e));
        });
    });
    group.bench_function("typescript", |b| {
        b.iter(|| {
            let (s, e) =
                extract_file_with_edges("a.ts", black_box(TYPESCRIPT_FIXTURE), "typescript")
                    .unwrap();
            black_box((s, e));
        });
    });

    group.finish();
}

fn bench_1000_files(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_1000_files");
    // Indexing cadence: 13 k-file repo, ~1 s budget. 1 000 calls per
    // bench loops through the per-file path enough times that any
    // per-call allocation amplifier (Parser::new in the pre-pool
    // world, or its absence here) shows up as a clear delta.
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("rust", |b| {
        b.iter(|| {
            for _ in 0..1_000 {
                let (s, e) = extract_file_with_edges("a.rs", RUST_FIXTURE, "rust").unwrap();
                black_box((s, e));
            }
        });
    });
    group.bench_function("python", |b| {
        b.iter(|| {
            for _ in 0..1_000 {
                let (s, e) = extract_file_with_edges("a.py", PYTHON_FIXTURE, "python").unwrap();
                black_box((s, e));
            }
        });
    });

    group.finish();
}

criterion_group!(benches, bench_single, bench_1000_files);
criterion_main!(benches);
