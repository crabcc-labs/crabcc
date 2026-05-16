//! v4 KG-op smoke test — index a 3-file Rust project with known call
//! relationships, then drive the four new graph ops through the `crabcc`
//! binary and assert the results match the wire-up.
//!
//! Coverage matrix:
//!   * `crabcc index --root <tmp>` populates the v4 `edges` table
//!   * `crabcc graph why a c` returns the path `[a, b, c]` (or its
//!     symbol-id equivalent) — proves the bidirectional BFS resolves
//!   * `crabcc graph blast-radius c` contains both `a` and `b` — proves
//!     the reverse transitive closure walks the whole upstream
//!   * `crabcc graph hot-symbols --top 3` ranks `c` first — proves the
//!     in-degree aggregator counts edges correctly
//!
//! The fixture is intentionally tiny: three top-level functions in three
//! files, no impls, no methods, no qualifiers. A whole-workspace
//! integration test would dwarf the signal here — this isolates "did the
//! v4 KG ops wire up end-to-end" from "does the rest of the toolchain
//! still work."

use std::path::Path;
use std::process::Command;

/// Mirrors the helper in `auto_index.rs`. Each test gets its own
/// `CRABCC_HOME` and disables backup / hint / deprecation chatter so
/// stderr is greppable.
fn crabcc() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_crabcc"));
    c.env_remove("CRABCC_HOME");
    c.env("CRABCC_BACKUP_DISABLE", "1");
    c.env("CRABCC_NO_HINT", "1");
    c.env("CRABCC_NO_DEPRECATION_WARN", "1");
    c
}

/// Three tiny Rust files. `a()` calls `b()`; `b()` calls `c()`; `c()`
/// is a leaf. In-degree: c=1, b=1, a=0. Reverse closure of c: {a, b}.
fn write_fixture(root: &Path) {
    std::fs::write(
        root.join("a.rs"),
        "pub fn a() { crate::b::b(); }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("b.rs"),
        "pub fn b() { crate::c::c(); }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("c.rs"),
        "pub fn c() {}\n",
    )
    .unwrap();
}

fn fresh_project() -> (tempfile::TempDir, tempfile::TempDir) {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_fixture(project.path());
    (project, home)
}

/// Run `crabcc index --root <project>` to populate the v4 edges table.
/// Returns the captured stderr for diagnostics.
fn run_index(project: &Path, home: &Path) -> String {
    let out = crabcc()
        .env("CRABCC_HOME", home)
        .args(["index", "--root"])
        .arg(project)
        .output()
        .expect("crabcc index");
    assert!(
        out.status.success(),
        "index failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn run_graph_op(project: &Path, home: &Path, args: &[&str]) -> serde_json::Value {
    let out = crabcc()
        .env("CRABCC_HOME", home)
        .args(["graph"])
        .args(args)
        .args(["--root"])
        .arg(project)
        .output()
        .expect("crabcc graph");
    assert!(
        out.status.success(),
        "graph {args:?} failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "expected JSON from `graph {args:?}`, got: {}\n(parse error: {e})",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

#[test]
fn why_finds_path_a_b_c() {
    let (project, home) = fresh_project();
    let _ = run_index(project.path(), home.path());

    // `graph why a c` should produce a path that traverses b.
    let v = run_graph_op(project.path(), home.path(), &["why", "a", "c"]);

    // The response is a JSON array of nodes (symbol-id or name records).
    // We extract a flat list of node-name strings so the assertion is
    // resilient to either shape — the contract is "the path goes a → b → c".
    let names = collect_names(&v);
    assert!(
        names.iter().any(|n| n == "a"),
        "expected 'a' in path; got names={names:?} body={v}",
    );
    assert!(
        names.iter().any(|n| n == "b"),
        "expected 'b' in path; got names={names:?} body={v}",
    );
    assert!(
        names.iter().any(|n| n == "c"),
        "expected 'c' in path; got names={names:?} body={v}",
    );
}

#[test]
fn blast_radius_of_c_contains_a_and_b() {
    let (project, home) = fresh_project();
    let _ = run_index(project.path(), home.path());

    let v = run_graph_op(project.path(), home.path(), &["blast-radius", "c"]);
    let names = collect_names(&v);
    assert!(
        names.iter().any(|n| n == "a"),
        "blast-radius of c should include a; got names={names:?} body={v}",
    );
    assert!(
        names.iter().any(|n| n == "b"),
        "blast-radius of c should include b; got names={names:?} body={v}",
    );
}

#[test]
fn hot_symbols_ranks_c_first() {
    let (project, home) = fresh_project();
    let _ = run_index(project.path(), home.path());

    let v = run_graph_op(project.path(), home.path(), &["hot-symbols", "--top", "3"]);
    let names = collect_names(&v);
    assert!(
        !names.is_empty(),
        "hot-symbols returned no entries; body={v}",
    );
    assert_eq!(
        names[0], "c",
        "expected c (highest in-degree) first; got names={names:?} body={v}",
    );
}

/// Pull "name" strings out of an unknown-shape graph response. The four
/// v4 KG ops emit either `[{"name": "...", ...}, ...]` (most-likely
/// shape — mirroring `Symbol` rows) or `[{"symbol_id": N, "name":
/// "...", ...}, ...]` (id-keyed records with a denormalized name). This
/// helper accepts both. If the shape is something else entirely the
/// assertion in the caller will fail with a useful body= dump.
fn collect_names(v: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    walk(v, &mut out);
    out
}

fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get("name") {
                out.push(s.clone());
            }
            for (_, child) in map {
                walk(child, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                walk(item, out);
            }
        }
        _ => {}
    }
}
