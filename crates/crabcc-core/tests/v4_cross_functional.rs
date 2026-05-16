//! Cross-functional v4 integration tests.
//!
//! These exercise the full pipeline that lands in production: tree-sitter
//! extract → `Store::replace_symbols` + `Store::replace_edges` (v4 column
//! port) → `query::*` + the new KG ops.  Each test stands up a fresh
//! tempdir-rooted repo, runs `index::full_index`, and then asserts the v4
//! store + query surface against the expected wire-up.
//!
//! Scope justification: the v4 work landed in three layers
//!   (1) schema + per-language resolvers (in isolation tested via
//!       crate-local unit tests),
//!   (2) `Store` API + KG ops (tested via the existing `tests/v4_regression.rs`),
//!   (3) the production indexer wiring (tested HERE).
//! This file covers layer (3): the seams between layers (1)/(2) and the
//! rest of the workspace.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crabcc_core::index;
use crabcc_core::query;
use crabcc_core::store::Store;

// ────────────────────────────────────────────────────────────────────────
// fixtures
// ────────────────────────────────────────────────────────────────────────

fn write(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir -p");
    }
    fs::write(&path, body).expect("write fixture");
    path
}

/// Three-file Rust chain: `a() → b() → c()`. In-degree: c=1, b=1, a=0.
/// Mirrors the fixture in `tests/integration/graph_v4.rs` so the same
/// invariants the CLI exercises also hold against the library API.
fn rust_chain_fixture() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    write(repo.path(), "a.rs", "pub fn a() { crate::b::b(); }\n");
    write(repo.path(), "b.rs", "pub fn b() { crate::c::c(); }\n");
    write(repo.path(), "c.rs", "pub fn c() {}\n");
    repo
}

fn fresh_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("idx.db")).unwrap();
    (dir, store)
}

// ────────────────────────────────────────────────────────────────────────
// polyglot
// ────────────────────────────────────────────────────────────────────────

/// Indexing must accept multi-language repos. We don't assert exact edge
/// counts (the per-language resolver wiring — CRIT-5 — is still deferred
/// to v4.0.1, so cross-language edges land as sentinels), but we DO assert
/// that every supported language contributes symbols and that the lang
/// column is populated in `files`.
#[test]
fn polyglot_index_recognises_rust_python_typescript() {
    let repo = tempfile::tempdir().unwrap();
    write(repo.path(), "src/lib.rs", "pub fn rusty() {}\n");
    write(repo.path(), "src/mod.py", "def pythonic():\n    pass\n");
    write(repo.path(), "src/web.ts", "export function tsy() {}\n");

    let (_dbdir, store) = fresh_store();
    let stats = index::full_index(repo.path(), &store).unwrap();

    assert!(
        stats.files_indexed >= 3,
        "expected ≥3 indexed files for polyglot fixture, got {stats:?}"
    );

    // Each top-level fn should land as a symbol.
    let langs: HashSet<String> = store
        .list_files()
        .unwrap()
        .into_iter()
        .map(|(_, lang)| lang)
        .collect();
    for required in ["rust", "python", "typescript"] {
        assert!(
            langs.contains(required),
            "language `{required}` missing from indexed files; got {langs:?}"
        );
    }

    let all_names: HashSet<String> = store
        .iter_all_symbols()
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    for required in ["rusty", "pythonic", "tsy"] {
        assert!(
            all_names.contains(required),
            "expected `{required}` symbol after polyglot index; got {all_names:?}"
        );
    }
}

// ────────────────────────────────────────────────────────────────────────
// KG ops on real `full_index` output
// ────────────────────────────────────────────────────────────────────────

fn id_of(store: &Store, name: &str) -> crabcc_core::resolve::SymbolId {
    store
        .symbol_id_by_name(name)
        .unwrap()
        .unwrap_or_else(|| panic!("symbol `{name}` not found in store"))
}

/// Currently fails because `full_index` indexes through `NameOnlyResolver`,
/// so every call edge writes a sentinel `dst_symbol_id` (`kind='sentinel'`,
/// `file_id = <unresolved>`) instead of the real callee's id. The BFS
/// can't find a path between real ids. Flips green once CRIT-5 lands.
#[test]
#[ignore = "CRIT-5: per-language resolvers not wired into full_index; deferred to v4.0.1"]
fn why_walks_real_full_index_chain() {
    let repo = rust_chain_fixture();
    let (_dbdir, store) = fresh_store();
    index::full_index(repo.path(), &store).unwrap();

    let a = id_of(&store, "a");
    let c = id_of(&store, "c");

    let path = query::why::why(&store, a, c, 4)
        .unwrap()
        .expect("expected a path from `a` to `c` through `b`");

    let names: Vec<String> = path.nodes.iter().map(|s| s.name.clone()).collect();
    assert!(
        names.first().map(String::as_str) == Some("a"),
        "path should start at `a`; got {names:?}"
    );
    assert!(
        names.last().map(String::as_str) == Some("c"),
        "path should end at `c`; got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "b"),
        "path should traverse `b`; got {names:?}"
    );
}

/// Same CRIT-5 blocker as `why_walks_real_full_index_chain`: edges land
/// on sentinels, so the reverse closure from a real leaf id is empty.
#[test]
#[ignore = "CRIT-5: per-language resolvers not wired into full_index; deferred to v4.0.1"]
fn blast_radius_of_real_full_index_leaf() {
    let repo = rust_chain_fixture();
    let (_dbdir, store) = fresh_store();
    index::full_index(repo.path(), &store).unwrap();

    let c = id_of(&store, "c");
    let result = query::blast_radius::blast_radius(&store, c, 4, &[]).unwrap();

    let names: HashSet<String> = result.affected.iter().map(|s| s.name.clone()).collect();
    assert!(
        names.contains("a"),
        "blast_radius(c) should reach `a` transitively; got {names:?}"
    );
    assert!(
        names.contains("b"),
        "blast_radius(c) should reach `b` (direct caller); got {names:?}"
    );
    assert!(
        !names.contains("c"),
        "blast_radius excludes the root itself; got {names:?}"
    );
}

/// CRIT-5: until resolvers land, the top-ranked rows in `hot_symbols` are
/// sentinels (file=`<unresolved>`) because the indexer routes every edge
/// through `NameOnlyResolver`. We pin the *fixed* contract here so this
/// test flips green the moment the wiring is done.
#[test]
#[ignore = "CRIT-5: per-language resolvers not wired into full_index; deferred to v4.0.1"]
fn hot_symbols_ranks_leaf_first_on_real_index() {
    let repo = rust_chain_fixture();
    let (_dbdir, store) = fresh_store();
    index::full_index(repo.path(), &store).unwrap();

    // Restrict to `call` kind so we ignore any `ref` edges the extractor
    // also emits — call in-degree is what we actually care about here.
    let hot = query::hot_symbols::hot_symbols(&store, 3, &["call"]).unwrap();
    assert!(!hot.is_empty(), "hot_symbols returned no entries");
    let leading = hot[0].symbol.name.as_str();
    assert_eq!(
        leading, "c",
        "expected `c` (in-degree 1, leaf of a→b→c chain) first; got {hot:?}"
    );
}

// ────────────────────────────────────────────────────────────────────────
// legacy query path — the one CRIT-2 broke
// ────────────────────────────────────────────────────────────────────────

/// `query::query_callers` routes through `Store::callers_of`, which was
/// querying dropped v3 columns until the v4 port (commit `d0f8288`). This
/// pins the contract: callers of a leaf must be findable after a real
/// `full_index`, end-to-end. We exercise the edges-fast path (which the
/// v4 port unblocked) by going through the public entry point.
#[test]
fn query_callers_resolves_after_full_index() {
    let repo = rust_chain_fixture();
    let (_dbdir, store) = fresh_store();
    index::full_index(repo.path(), &store).unwrap();

    let output =
        query::query_callers(&store, repo.path(), "c", query::Mode::default(), None).unwrap();
    let snippets: Vec<String> = match &output {
        query::Output::Hits(rows) => rows.iter().map(|h| h.snippet.clone()).collect(),
        other => panic!("expected Output::Hits, got {other:?}"),
    };
    assert!(
        !snippets.is_empty(),
        "no callers of `c` after full_index; got output={output:?}"
    );
    assert!(
        snippets.iter().any(|s| s.contains("c()")),
        "expected at least one caller snippet referencing `c()`; got {snippets:?}"
    );
}

// ────────────────────────────────────────────────────────────────────────
// refresh — file-edit round-trip
// ────────────────────────────────────────────────────────────────────────

/// `refresh_delta` is the live-watching path. Editing a file in place must
/// drop the stale symbols and pick up the new ones — symbol-id continuity
/// is not guaranteed, but the name-keyed query surface must reflect the
/// new content immediately.
#[test]
fn refresh_delta_replaces_symbols_after_edit() {
    let repo = tempfile::tempdir().unwrap();
    let lib = write(repo.path(), "lib.rs", "pub fn old_name() {}\n");
    let (_dbdir, store) = fresh_store();
    index::full_index(repo.path(), &store).unwrap();

    assert!(store.symbol_id_by_name("old_name").unwrap().is_some());
    assert!(store.symbol_id_by_name("new_name").unwrap().is_none());

    // Edit the file: rename the function and bump mtime. Force mtime
    // forward via `FileTimes` so the staleness check fires deterministically
    // on filesystems with second-resolution mtimes.
    fs::write(&lib, "pub fn new_name() {}\n").unwrap();
    let f = fs::File::options().write(true).open(&lib).unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    let times = std::fs::FileTimes::new().set_modified(later);
    f.set_times(times).unwrap();
    drop(f);

    let delta = index::refresh_delta(repo.path(), &store).unwrap();
    assert!(
        delta.modified.iter().any(|p| p.ends_with("lib.rs")),
        "refresh_delta did not mark lib.rs as modified; got {delta:?}"
    );

    assert!(
        store.symbol_id_by_name("old_name").unwrap().is_none(),
        "stale symbol `old_name` survived refresh"
    );
    assert!(
        store.symbol_id_by_name("new_name").unwrap().is_some(),
        "new symbol `new_name` not visible after refresh"
    );
}

// ────────────────────────────────────────────────────────────────────────
// FK cascade — schema contract
// ────────────────────────────────────────────────────────────────────────

/// The v4 schema declares
/// `edges.src_symbol_id REFERENCES symbols(id) ON DELETE CASCADE` and
/// `symbols.file_id REFERENCES files(id) ON DELETE CASCADE`. Deleting a
/// file must propagate through symbols all the way to edges, with no
/// manual cleanup. If a future schema migration drops the CASCADE this
/// test fires before the next reviewer notices.
#[test]
fn delete_file_cascades_symbols_and_edges() {
    let repo = rust_chain_fixture();
    let (_dbdir, store) = fresh_store();
    index::full_index(repo.path(), &store).unwrap();

    // Capture the *real* `b` symbol's id — the one rooted in `b.rs`, not
    // the sentinel that CRIT-5 (still active) creates under `<unresolved>`.
    // We query by (name, file) to disambiguate.
    let b_file_id: i64 = store
        .conn()
        .query_row("SELECT id FROM files WHERE path = 'b.rs'", [], |r| r.get(0))
        .unwrap();
    let b_id_before = store
        .symbol_id_by_name_file("b", b_file_id)
        .unwrap()
        .expect("precondition: real `b` symbol should exist before delete");
    let b_raw = b_id_before.into_raw();

    // Drop b.rs — `files.id` cascades through `symbols.file_id` and from
    // there through `edges.{src_symbol_id, dst_symbol_id}`.
    store.delete_file("b.rs").unwrap();

    // The specific real-b row id must be gone (not just absence-by-name —
    // a sentinel `b` may still live under `<unresolved>`).
    let still_present: bool = store
        .conn()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM symbols WHERE id = ?1)",
            [b_raw],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        !still_present,
        "real `b` symbol (id={b_raw}) survived delete_file(b.rs); FK cascade is broken"
    );

    // No edge in the store should still reference that specific deleted
    // symbol id on either end.
    let edges = store.iter_call_edges_resolved().unwrap();
    let orphan = edges
        .iter()
        .filter(|e| e.src == b_id_before || e.dst == b_id_before)
        .count();
    assert_eq!(
        orphan, 0,
        "edges referencing deleted symbol id {b_raw} remain: {edges:?}"
    );
}
