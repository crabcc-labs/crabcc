//! Agent-usage MCP benchmark — the agent→MCP hot path.
//!
//! `serve_io.rs` measures only the I/O envelope (a `tools/list` batch with no
//! filesystem touches). This bench exercises **real tool dispatch** the way a
//! CLI coding agent does: it builds an index over this crate's own source,
//! synthesizes per-agent tool-call workloads (sym / refs / callers / outline /
//! fuzzy / prefix / read / ctx) from real symbol + file names, and drives them
//! through the production `serve_io` loop.
//!
//! Profiles (see `agent_profiles.rs`): `claude_code`, `nullclaw`, `zeroclaw`.
//!
//! Run:
//!     cargo bench -p crabcc-mcp --features bench --bench agent_workload
//!
//! The end-to-end counterpart (a real child process over stdio) lives in
//! `examples/agent_replay.rs`.

use std::hint::black_box;
use std::io::Cursor;
use std::path::Path;

use crabcc_core::store::Store;
use crabcc_mcp::{handle_with, serve_io};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};

#[path = "agent_profiles.rs"]
mod agent_profiles;
use agent_profiles::Profile;

/// Tool calls per synthesized workload. Matches the 100-message scale of
/// `serve_io.rs` so the two benches are comparable.
const CALLS: usize = 120;

/// Recursively copy `src` into `dst` (files only), preserving structure.
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap().flatten() {
        let ty = entry.file_type().unwrap();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_tree(&entry.path(), &to);
        } else if ty.is_file() {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// Build a real index over a copy of this crate's `src/` in a tempdir and
/// return the dir (kept alive for the index db) plus discovered files + syms.
fn setup() -> (tempfile::TempDir, Vec<String>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    copy_tree(&src, dir.path());
    // Store::open creates the db file but not its parent — mirror fixture_root().
    std::fs::create_dir_all(dir.path().join(".crabcc")).unwrap();
    let store = Store::open(&dir.path().join(".crabcc").join("index.db")).unwrap();
    crabcc_core::index::full_index(dir.path(), &store).unwrap();
    drop(store);
    let (files, syms) = agent_profiles::discover(dir.path());
    (dir, files, syms)
}

fn bench_agent_workload(c: &mut Criterion) {
    let (dir, files, syms) = setup();
    let root = dir.path();

    // Full per-agent workloads driven through the real serve_io loop.
    let mut group = c.benchmark_group("agent_workload");
    for profile in Profile::ALL {
        let traffic = agent_profiles::synthesize(profile, &syms, &files, CALLS);
        group.throughput(Throughput::Elements(CALLS as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(profile.name()),
            &traffic,
            |b, traffic| {
                b.iter_batched(
                    || {
                        (
                            Cursor::new(traffic.clone()),
                            Vec::<u8>::with_capacity(256 * 1024),
                        )
                    },
                    |(reader, mut writer)| {
                        serve_io(reader, &mut writer, black_box(root), false).unwrap();
                        black_box(writer);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();

    // Per-tool attribution: single-call handle_with cost for the hot tools.
    let mut tools = c.benchmark_group("agent_tool");
    let sym = syms.first().cloned().unwrap_or_else(|| "main".into());
    let file = files.first().cloned().unwrap_or_else(|| "lib.rs".into());
    let cases = [
        ("sym", serde_json::json!({ "name": sym })),
        ("refs", serde_json::json!({ "name": sym, "limit": 50 })),
        ("callers", serde_json::json!({ "name": sym })),
        ("outline", serde_json::json!({ "file": file })),
    ];
    for (tool, args) in cases {
        let req = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        });
        tools.bench_function(tool, |b| {
            b.iter(|| black_box(handle_with(black_box(&req), black_box(root), false)));
        });
    }
    tools.finish();

    drop(dir);
}

criterion_group!(benches, bench_agent_workload);
criterion_main!(benches);
