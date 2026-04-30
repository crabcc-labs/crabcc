//! Benchmark — issue #89 slice 1.
//!
//! Compares the optimised `serve_io` (read_until + from_slice +
//! to_writer + reused Vec<u8>) against a baseline that mimics the old
//! `read_line + writeln!("{value}")` shape. Same JSON-RPC traffic, same
//! tool surface, same `handle_with` — only the I/O layer differs.
//!
//! Run with:
//!     cargo bench -p crabcc-mcp --bench serve_io
//!
//! The reported number is **time per request** (not throughput) on a
//! 100-message NDJSON batch of `tools/list` requests. `tools/list` is
//! a good probe target because it exercises the full encode-decode
//! envelope without any filesystem touches (no Store::open, no MCP
//! tool dispatch beyond a single `serde_json::to_value(tools_def)`).

use std::io::{BufRead, Cursor, Write};

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use crabcc_mcp::{handle_with, serve_io};
use serde_json::{json, Value};

/// 100 newline-delimited JSON-RPC requests — the workload both
/// implementations parse + dispatch + serialise.
fn workload() -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 * 1024);
    for i in 0..100 {
        let req = json!({
            "jsonrpc": "2.0",
            "id": i,
            "method": "tools/list",
            "params": {},
        });
        buf.extend_from_slice(req.to_string().as_bytes());
        buf.push(b'\n');
    }
    buf
}

/// Baseline serve loop — the shape from BEFORE the optimization.
/// `read_line` + `String::trim()` + `writeln!("{value}")`. Inlined
/// here so the bench can A/B without time-traveling through git.
fn serve_io_baseline<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    root: &std::path::Path,
    dev: bool,
) -> std::io::Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line)? {
            0 => break,
            _ => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let resp = handle_with(&req, root, dev);
        if resp.is_null() {
            continue;
        }
        // The expensive line: `Display` on Value goes through
        // `Value::to_string()` → allocates a fresh String each call.
        writeln!(writer, "{resp}")?;
    }
    Ok(())
}

fn bench_serve_io(c: &mut Criterion) {
    let traffic = workload();
    let root = std::env::temp_dir();
    let mut group = c.benchmark_group("serve_io");
    group.throughput(Throughput::Bytes(traffic.len() as u64));

    group.bench_function("optimized", |b| {
        b.iter_batched(
            || (Cursor::new(traffic.clone()), Vec::<u8>::with_capacity(64 * 1024)),
            |(reader, mut writer)| {
                serve_io(reader, &mut writer, black_box(&root), false).unwrap();
                black_box(writer);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("baseline_read_line_writeln", |b| {
        b.iter_batched(
            || (Cursor::new(traffic.clone()), Vec::<u8>::with_capacity(64 * 1024)),
            |(reader, mut writer)| {
                serve_io_baseline(reader, &mut writer, black_box(&root), false).unwrap();
                black_box(writer);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_serve_io);
criterion_main!(benches);
