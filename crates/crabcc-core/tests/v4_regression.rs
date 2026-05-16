//! Regression tests for the v4-hackathon critical findings.
//!
//! Each test pins a specific bug from the review at
//! `docs/superpowers/reviews/v4-hackathon/`. They are expected to fail on the
//! current `v4-hackathon` HEAD and pass once the corresponding fix lands.
//!
//! See `00-EXECUTIVE-SUMMARY.md` in the review folder for the full table.

use std::fs;
use std::path::PathBuf;

use crabcc_core::extract;
use crabcc_core::index;
use crabcc_core::resolve::NameOnlyResolver;
use crabcc_core::store::Store;

fn fresh_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("idx.db");
    let store = Store::open(&db).expect("open store");
    (dir, store)
}

fn write_rust_file(dir: &std::path::Path, rel: &str, body: &str) -> PathBuf {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir -p");
    }
    fs::write(&path, body).expect("write file");
    path
}

/// CRIT-2 (integration): `index::full_index` must run end-to-end without the
/// `no such column` panic. Today the indexer routes through `replace_edges`,
/// which still issues SQL against the dropped v3 columns and aborts every
/// re-index attempt. Forty test failures in `cargo test` originate here.
#[test]
fn full_index_completes_on_a_rust_file() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    write_rust_file(
        repo.path(),
        "src/lib.rs",
        r#"
            pub fn caller() {
                callee();
            }
            pub fn callee() {}
        "#,
    );

    let dbdir = tempfile::tempdir().expect("db tempdir");
    let store = Store::open(&dbdir.path().join("idx.db")).expect("open store");

    let stats = index::full_index(repo.path(), &store).expect(
        "full_index must not fail on a single-file Rust repo; \
         current v4 store.replace_edges queries dropped v3 columns",
    );

    assert!(
        stats.files_indexed >= 1,
        "expected at least one indexed file, got {stats:?}"
    );
    assert!(
        stats.symbols >= 2,
        "expected caller + callee symbols, got {stats:?}"
    );
}

/// CRIT-5: the per-language resolvers (`RustResolver`, `TsResolver`,
/// `PythonResolver`) must be reachable from the indexer. Today no code path
/// in `index::full_index` constructs them; production indexing routes through
/// `NameOnlyResolver`, so every edge writes a sentinel `dst_symbol_id`.
///
/// We exercise this by indexing a self-contained Rust file with an intra-file
/// call (`caller -> callee`) and asserting the resulting edge points at the
/// real `callee` symbol, not a sentinel. While the resolvers stay unwired this
/// test fails — the edge's `dst_symbol_id` resolves to a row with
/// `kind = 'sentinel'`.
#[test]
fn full_index_resolves_intra_file_call_to_real_symbol() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    write_rust_file(
        repo.path(),
        "src/lib.rs",
        r#"
            pub fn caller() {
                callee();
            }
            pub fn callee() {}
        "#,
    );

    let dbdir = tempfile::tempdir().expect("db tempdir");
    let store = Store::open(&dbdir.path().join("idx.db")).expect("open store");
    index::full_index(repo.path(), &store).expect("full_index");

    // Pull every (src, dst, kind) tuple back out of the v4 edges table joined
    // to the symbol table on both ends. A correctly resolved call shows up as
    // {src_name='caller', dst_name='callee', dst_kind != 'sentinel'}.
    let conn = store.conn();
    let mut stmt = conn
        .prepare(
            "SELECT s.name, d.name, d.kind
             FROM edges e
             JOIN symbols s ON s.id = e.src_symbol_id
             JOIN symbols d ON d.id = e.dst_symbol_id
             WHERE e.kind = 'call'",
        )
        .expect("prepare edges select");
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .expect("query_map")
        .collect::<Result<_, _>>()
        .expect("collect rows");

    let resolved = rows
        .iter()
        .find(|(src, dst, kind)| src == "caller" && dst == "callee" && kind != "sentinel");

    assert!(
        resolved.is_some(),
        "no resolved `caller -> callee` edge found; full set was {rows:?}. \
         If every dst.kind == 'sentinel', the per-language Rust resolver is \
         not wired into full_index (CRIT-5)."
    );
}

/// CRIT-4: when the two-pass extractor reaches `insert_symbol` with
/// `file_id = 0` (because the file was never `upsert_file`'d), the current
/// code swallows the FK-violation via `.unwrap_or(-1)` and drops the symbol
/// silently. The next reference to that symbol then writes a sentinel edge,
/// with no error surfaced to the caller. We pin the contract: passing a
/// store that does not know about the file must produce a hard error, never
/// silent data loss.
#[test]
fn extract_with_store_errors_on_missing_file_id() {
    let (_dir, store) = fresh_store();

    // Deliberately skip `store.upsert_file("a.rs", ...)`. The two-pass walker
    // will try to look up the file_id, fall back to 0, and INSERT against an
    // FK that does not resolve.
    let src = r#"
        pub fn caller() {
            callee();
        }
        pub fn callee() {}
    "#;

    let result = extract::extract_file_with_edges_with_resolver(
        "a.rs",
        src,
        "rust",
        &store,
        &NameOnlyResolver,
    );

    assert!(
        result.is_err(),
        "extract_file_with_edges_with_resolver must surface the FK violation \
         when the file is not pre-upserted; currently it returns Ok with the \
         dropped symbols silently turned into sentinels."
    );
}
