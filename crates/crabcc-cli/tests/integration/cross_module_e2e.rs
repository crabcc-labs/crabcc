//! Cross-module integration: drive a single indexed fixture through the
//! whole query surface (index -> store -> query -> graph -> FTS) via the
//! `crabcc` binary and assert the modules cohere on the same symbols.
//! Also covers the empty-result edge cases that must never panic.

use std::path::Path;
use std::process::Command;

fn crabcc() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_crabcc"));
    c.env_remove("CRABCC_HOME");
    c.env("CRABCC_BACKUP_DISABLE", "1");
    c.env("CRABCC_NO_HINT", "1");
    c.env("CRABCC_NO_DEPRECATION_WARN", "1");
    c
}

/// Two-file Rust fixture with a real cross-file call graph:
/// `helper -> run -> Store::open` + a `Store` reference in `run`.
fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("store.rs"),
        "pub struct Store { conn: u32 }\n\
         impl Store {\n  pub fn open() -> Store { Store { conn: 0 } }\n  \
         pub fn query(&self) -> u32 { self.conn }\n}\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("app.rs"),
        // `-> Store` emits a type-ref edge to Store (cross-file); helper -> run
        // is the call edge the callers/graph queries resolve.
        "fn run() -> Store {\n  Store::open()\n}\n\
         fn helper() -> u32 { run().query() }\n",
    )
    .unwrap();
    let idx = crabcc()
        .env("CRABCC_HOME", home.path())
        .args(["index", "--root"])
        .arg(project.path())
        .output()
        .unwrap();
    assert!(
        idx.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&idx.stderr)
    );
    (project, home)
}

/// Run a query subcommand against the fixture; assert exit 0 and return stdout.
fn run(project: &Path, home: &Path, args: &[&str]) -> String {
    let out = crabcc()
        .env("CRABCC_HOME", home)
        .args(args)
        .arg("--root")
        .arg(project)
        .output()
        .unwrap_or_else(|e| panic!("spawn {args:?}: {e}"));
    assert!(
        out.status.success(),
        "{args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn query_surface_coheres_on_one_index() {
    let (p, h) = fixture();
    let (p, h) = (p.path(), h.path());

    // sym: the struct definition resolves to store.rs.
    let sym = run(p, h, &["lookup", "sym", "Store"]);
    let v: serde_json::Value = serde_json::from_str(&sym).unwrap();
    assert!(
        v.as_array().is_some_and(|a| a
            .iter()
            .any(|s| s["name"] == "Store" && s["file"] == "store.rs")),
        "sym Store should resolve to store.rs: {sym}"
    );

    // refs: Store is referenced from app.rs (Store::open + return type).
    assert!(
        run(p, h, &["lookup", "refs", "Store"]).contains("app.rs"),
        "refs Store should reach app.rs"
    );

    // callers: helper calls run (cross-fn edge, SQL fast path after index).
    let callers = run(p, h, &["lookup", "callers", "run"]);
    assert!(
        callers.contains("helper"),
        "callers run should include helper: {callers}"
    );

    // callers --count: the count surface agrees there is >= 1 caller.
    let count = run(p, h, &["lookup", "callers", "run", "--count"]);
    let cv: serde_json::Value = serde_json::from_str(&count).unwrap();
    assert!(cv["count"].as_i64().unwrap_or(0) >= 1, "count: {count}");

    // FTS: fuzzy (Levenshtein-2) finds Store from a typo.
    assert!(
        run(p, h, &["lookup", "fuzzy", "Stroe"]).contains("Store"),
        "fuzzy should find Store"
    );

    // files: the indexed file list includes both sources.
    let files = run(p, h, &["lookup", "files", "--ext", "rs"]);
    assert!(
        files.contains("store.rs") && files.contains("app.rs"),
        "files: {files}"
    );

    // graph: build the call graph, then walk callers of run -> finds a
    // caller node (helper, by symbol_id) at depth 1.
    let _ = run(p, h, &["graph", "build"]);
    let walk = run(
        p,
        h,
        &["graph", "walk", "run", "--dir", "callers", "--depth", "2"],
    );
    let wv: serde_json::Value = serde_json::from_str(&walk).unwrap();
    assert!(
        wv.as_array()
            .is_some_and(|a| a.iter().any(|n| n["depth"].as_i64() == Some(1))),
        "graph walk callers(run) should find a depth-1 caller: {walk}"
    );
}

#[test]
fn empty_results_never_panic() {
    let (p, h) = fixture();
    let (p, h) = (p.path(), h.path());

    // A symbol that doesn't exist: every surface returns a clean empty
    // result (exit 0), not an error or panic.
    let sym = run(p, h, &["lookup", "sym", "DefinitelyNotARealSymbol"]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&sym)
            .unwrap()
            .as_array()
            .map(|a| a.len()),
        Some(0),
        "sym of unknown symbol must be an empty array: {sym}"
    );
    let count = run(
        p,
        h,
        &["lookup", "callers", "DefinitelyNotARealSymbol", "--count"],
    );
    let cv: serde_json::Value = serde_json::from_str(&count).unwrap();
    assert_eq!(
        cv["count"].as_i64(),
        Some(0),
        "count of unknown symbol: {count}"
    );
}
