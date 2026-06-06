//! `affected` — graph-derived change impact + targeted test selection.
//!
//! Given a change (working-tree diff, a `--since` git range, or an explicit
//! symbol set), resolve the changed symbols, walk the call graph UPWARD to
//! their transitive callers (`query::blast_radius`, which already walks
//! reverse edges), keep the callers that look like tests, and emit a
//! ready-to-run test command for the detected runner.
//!
//! This closes the agent loop's *verify* step: instead of rerunning the whole
//! suite after a one-symbol edit, run exactly the tests that transitively
//! exercise the change.
//!
//! Test detection is heuristic (the extractor does not record `#[test]`
//! attributes): a symbol is treated as a test when it is a function/method
//! and any of — it lives under a `tests/` path or a `*_test` / `*.spec`
//! filename, its name follows a test convention (`test_*`, Go `Test*`), or
//! its parent module is `tests`/`test` (Rust `#[cfg(test)] mod tests`, whose
//! file is *not* under `tests/`). See `is_test_symbol`.

use crate::gitdiff;
use crate::query::blast_radius::blast_radius;
use crate::store::{kind_from_str, Store};
use crate::types::{Symbol, SymbolKind};
use ahash::HashSet;
use anyhow::Result;
use rusqlite::params_from_iter;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Default upward-walk depth when the caller doesn't specify one.
pub const DEFAULT_DEPTH: usize = 3;

/// Edge kinds the upward walk follows. `call` and `ref` together favour
/// recall (a test that *references* a changed const/type counts as
/// exercising it); `import`/`inherit`/`impl` are excluded as too broad.
const WALK_KINDS: &[&str] = &["call", "ref"];

/// How the caller describes "the change".
#[derive(Debug, Clone)]
pub enum ChangeInput {
    /// Uncommitted + staged changes vs HEAD (the default agent-loop case).
    WorkingTree,
    /// Anything `git diff` accepts as `<rev>...HEAD` (SHA, ref, `HEAD~5`).
    Since(String),
    /// Explicit symbol names; skips git entirely.
    Symbols(Vec<String>),
}

/// A test that transitively exercises the change.
#[derive(Debug, Serialize)]
pub struct AffectedTest {
    pub name: String,
    pub file: String,
    pub line: u32,
    pub kind: SymbolKind,
    /// Hops from a changed symbol. 0 = the changed symbol is itself a test.
    pub via_depth: usize,
}

/// The full impact report for a change.
#[derive(Debug, Serialize)]
pub struct AffectedResult {
    /// Files considered (empty for the explicit-`Symbols` input).
    pub changed_files: Vec<String>,
    /// Symbols defined in the changed files (file-level granularity).
    pub changed_symbols: Vec<Symbol>,
    /// Tests selected, sorted by `(file, line)`.
    pub tests: Vec<AffectedTest>,
    /// Detected test runner (`cargo`/`go`/`pytest`/`npm`), if any.
    pub runner: Option<String>,
    /// Ready-to-run command for `runner`, or `None` when no runner was
    /// detected or no tests were selected.
    pub command: Option<String>,
}

/// Resolve a change to the set of tests that transitively exercise it.
pub fn affected(
    store: &Store,
    root: &Path,
    input: ChangeInput,
    depth: usize,
) -> Result<AffectedResult> {
    // 1. Resolve changed symbols as `(id, Symbol)`.
    let (changed_files, changed): (Vec<String>, Vec<(i64, Symbol)>) = match input {
        ChangeInput::Symbols(names) => {
            let mut out = Vec::new();
            for n in &names {
                out.extend(resolve_by_name(store, n)?);
            }
            (Vec::new(), out)
        }
        ChangeInput::Since(rev) => {
            let files = sorted(gitdiff::changed_files_since(root, &rev)?);
            let syms = symbols_in_files(store, &files)?;
            (files, syms)
        }
        ChangeInput::WorkingTree => {
            let files = sorted(gitdiff::changed_files_worktree(root)?);
            let syms = symbols_in_files(store, &files)?;
            (files, syms)
        }
    };

    // 2. Candidate test ids: each changed symbol itself (depth 0, so a changed
    //    test is selected) plus its transitive callers/referrers
    //    (depth 1..=depth). We walk from EVERY changed symbol, not just
    //    callables: `WALK_KINDS` includes `ref`, so a changed const/type/struct
    //    expands to the tests that reference it. (`is_test_symbol` later filters
    //    the candidates down to actual test functions.)
    let mut cand: BTreeMap<i64, usize> = BTreeMap::new();
    for (id, _sym) in &changed {
        bump(&mut cand, *id, 0);
        let br = blast_radius(store, *id, depth, WALK_KINDS)?;
        for (aid, d) in br.depth_map {
            bump(&mut cand, aid, d);
        }
    }

    // 3. Hydrate candidates with parent-module info (needed to spot Rust
    //    `#[cfg(test)] mod tests` fns, whose file is NOT under tests/).
    let hydrated = hydrate(store, cand.keys().copied().collect())?;

    // 4. Keep the candidates that look like tests.
    let mut tests: Vec<AffectedTest> = hydrated
        .into_iter()
        .filter(|(_, s)| is_test_symbol(s))
        .map(|(id, s)| AffectedTest {
            via_depth: cand.get(&id).copied().unwrap_or(0),
            name: s.name,
            file: s.file,
            line: s.line_start,
            kind: s.kind,
        })
        .collect();
    tests.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

    // 5. Detect the runner and build the command.
    let runner = detect_runner(root);
    let command = build_command(runner.as_deref(), &tests);

    Ok(AffectedResult {
        changed_files,
        changed_symbols: changed.into_iter().map(|(_, s)| s).collect(),
        tests,
        runner,
        command,
    })
}

fn bump(m: &mut BTreeMap<i64, usize>, id: i64, d: usize) {
    m.entry(id)
        .and_modify(|e| {
            if d < *e {
                *e = d;
            }
        })
        .or_insert(d);
}

fn sorted(set: HashSet<String>) -> Vec<String> {
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort();
    v
}

fn symbols_in_files(store: &Store, files: &[String]) -> Result<Vec<(i64, Symbol)>> {
    let mut out = Vec::new();
    for f in files {
        out.extend(store.symbols_in_file_with_ids(f)?);
    }
    Ok(out)
}

/// Resolve every symbol matching `name` to `(id, Symbol)` (with parent).
fn resolve_by_name(store: &Store, name: &str) -> Result<Vec<(i64, Symbol)>> {
    let conn = store.conn();
    let mut stmt = conn.prepare_cached(
        "SELECT s.id, s.name, s.kind, p.name, f.path, s.line_start, s.line_end, s.visibility
         FROM symbols s
         JOIN files f ON s.file_id = f.id
         LEFT JOIN symbols p ON p.id = s.parent_id
         WHERE s.name = ?1 AND s.kind != 'sentinel'",
    )?;
    let rows = stmt.query_map([name], row_to_id_symbol)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Hydrate a set of symbol ids to `(id, Symbol)` (with parent, no signature).
fn hydrate(store: &Store, ids: Vec<i64>) -> Result<Vec<(i64, Symbol)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let conn = store.conn();
    let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("?{}", i + 1)).collect();
    let sql = format!(
        "SELECT s.id, s.name, s.kind, p.name, f.path, s.line_start, s.line_end, s.visibility
         FROM symbols s
         JOIN files f ON s.file_id = f.id
         LEFT JOIN symbols p ON p.id = s.parent_id
         WHERE s.kind != 'sentinel' AND s.id IN ({})",
        placeholders.join(",")
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<Box<dyn rusqlite::ToSql>> = ids
        .into_iter()
        .map(|i| Box::new(i) as Box<dyn rusqlite::ToSql>)
        .collect();
    let rows = stmt.query_map(
        params_from_iter(params.iter().map(|b| b.as_ref())),
        row_to_id_symbol,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn row_to_id_symbol(row: &rusqlite::Row) -> rusqlite::Result<(i64, Symbol)> {
    Ok((
        row.get(0)?,
        Symbol {
            name: row.get(1)?,
            kind: kind_from_str(&row.get::<_, String>(2)?),
            signature: None,
            parent: row.get(3)?,
            file: row.get(4)?,
            line_start: row.get(5)?,
            line_end: row.get(6)?,
            visibility: row.get(7)?,
        },
    ))
}

/// Heuristic: does this symbol look like a test? (No `#[test]` metadata in
/// the index, so we infer from kind + file path + name + parent module.)
fn is_test_symbol(s: &Symbol) -> bool {
    matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
        && (test_path(&s.file) || test_name(&s.name, &s.file) || test_parent(s.parent.as_deref()))
}

/// File lives under a `tests/`/`test/` dir, or has a test-file basename.
fn test_path(path: &str) -> bool {
    if path.split('/').any(|seg| seg == "tests" || seg == "test") {
        return true;
    }
    let base = path.rsplit('/').next().unwrap_or(path);
    base.ends_with("_test.rs")
        || base.ends_with("_test.go")
        || base.ends_with("_test.py")
        || (base.starts_with("test_") && base.ends_with(".py"))
        || base.ends_with(".test.ts")
        || base.ends_with(".test.tsx")
        || base.ends_with(".test.js")
        || base.ends_with(".test.jsx")
        || base.ends_with(".spec.ts")
        || base.ends_with(".spec.js")
}

/// Name follows a test convention.
fn test_name(name: &str, file: &str) -> bool {
    name.starts_with("test_")
        || (file.ends_with("_test.go")
            && (name.starts_with("Test") || name.starts_with("Benchmark")))
}

/// Parent module is a Rust test module (`mod tests` / `mod test`).
fn test_parent(parent: Option<&str>) -> bool {
    match parent {
        Some(p) => {
            let p = p.to_ascii_lowercase();
            p == "test" || p == "tests" || p.ends_with("_test") || p.ends_with("_tests")
        }
        None => false,
    }
}

/// Detect the project's test runner from manifest files at `root`.
fn detect_runner(root: &Path) -> Option<String> {
    let has = |f: &str| root.join(f).exists();
    if has("Cargo.toml") {
        Some("cargo".into())
    } else if has("go.mod") {
        Some("go".into())
    } else if has("pyproject.toml") || has("pytest.ini") || has("setup.cfg") || has("tox.ini") {
        Some("pytest".into())
    } else if has("package.json") {
        Some("npm".into())
    } else {
        None
    }
}

/// Build a ready-to-run, filtered test command for `runner`.
fn build_command(runner: Option<&str>, tests: &[AffectedTest]) -> Option<String> {
    if tests.is_empty() {
        return None;
    }
    let mut names: Vec<&str> = tests.iter().map(|t| t.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    match runner? {
        // libtest treats multiple free args as an OR filter, each a SUBSTRING
        // match on the test path — so selection may over-include (still far
        // smaller than the whole suite), which suits this heuristic tool.
        "cargo" => Some(format!("cargo test -- {}", names.join(" "))),
        "go" => Some(format!("go test ./... -run '^({})$'", names.join("|"))),
        "pytest" => Some(format!("pytest -k \"{}\"", names.join(" or "))),
        "npm" => Some(format!("npm test -- -t \"{}\"", names.join("|"))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    /// Fixture mirroring a real Rust layout:
    ///   src/core.rs:   core_fn (the change)
    ///   src/widget.rs: `mod tests` (Class "tests") + unit_test (parent "tests")
    ///   tests/it.rs:   integ_test (integration test, under tests/)
    ///   src/app.rs:    caller (a NON-test fn that also calls core_fn)
    ///   src/core.rs:   MAX (a const the unit test references)
    /// edges: unit_test/integ_test/caller -call-> core_fn; unit_test -ref-> MAX
    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("idx.db")).unwrap();
        let conn = store.conn();
        conn.execute(
            "INSERT INTO files(path, sha256, mtime, lang, indexed_at) VALUES \
             ('src/core.rs','a',0,'rust',0), \
             ('src/widget.rs','b',0,'rust',0), \
             ('tests/it.rs','c',0,'rust',0), \
             ('src/app.rs','d',0,'rust',0)",
            [],
        )
        .unwrap();
        // id 10 = `mod tests` (Class). unit_test (id 2) has parent_id 10.
        conn.execute(
            "INSERT INTO symbols(id, file_id, name, kind, line_start, line_end, parent_id) VALUES \
             (1, 1, 'core_fn',   'function', 1, 5, NULL), \
             (10, 2, 'tests',    'class',   10, 30, NULL), \
             (2, 2, 'unit_test', 'function', 12, 16, 10), \
             (3, 3, 'integ_test','function', 1, 6, NULL), \
             (4, 4, 'caller',    'function', 1, 6, NULL), \
             (20, 1, 'MAX',      'const',    2, 2, NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges(src_symbol_id, dst_symbol_id, kind, line) VALUES \
             (2, 1, 'call', 13), \
             (3, 1, 'call', 2), \
             (4, 1, 'call', 2), \
             (2, 20, 'ref', 14)",
            [],
        )
        .unwrap();
        (dir, store)
    }

    #[test]
    fn selects_unit_and_integration_tests_excludes_plain_caller() {
        let (dir, store) = fixture();
        let r = affected(
            &store,
            dir.path(),
            ChangeInput::Symbols(vec!["core_fn".into()]),
            3,
        )
        .unwrap();
        let names: Vec<&str> = r.tests.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"unit_test"),
            "unit_test (mod tests) selected: {names:?}"
        );
        assert!(
            names.contains(&"integ_test"),
            "integ_test (tests/ dir) selected: {names:?}"
        );
        assert!(
            !names.contains(&"caller"),
            "plain caller must NOT be selected: {names:?}"
        );
        assert!(
            !names.contains(&"core_fn"),
            "the change itself is not a test: {names:?}"
        );
    }

    #[test]
    fn selects_tests_referencing_a_changed_const() {
        // Regression: a changed NON-callable (const MAX) must expand to the
        // tests that reference it via `ref` edges. The earlier callables-only
        // filter skipped consts entirely, so this returned no tests.
        let (dir, store) = fixture();
        let r = affected(
            &store,
            dir.path(),
            ChangeInput::Symbols(vec!["MAX".into()]),
            3,
        )
        .unwrap();
        let names: Vec<&str> = r.tests.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"unit_test"),
            "test referencing changed const MAX must be selected: {names:?}"
        );
    }

    #[test]
    fn parent_module_detects_rust_unit_test() {
        // The dominant Rust pattern: #[test] fn inside `mod tests`, in a src
        // file that is NOT under tests/. Only the parent-module signal catches it.
        let s = Symbol {
            name: "open_wal".into(),
            kind: SymbolKind::Function,
            signature: None,
            parent: Some("tests".into()),
            file: "crates/crabcc-core/src/store.rs".into(),
            line_start: 900,
            line_end: 910,
            visibility: None,
        };
        assert!(is_test_symbol(&s));
    }

    #[test]
    fn non_test_function_is_not_selected() {
        let s = Symbol {
            name: "open".into(),
            kind: SymbolKind::Function,
            signature: None,
            parent: Some("Store".into()),
            file: "crates/crabcc-core/src/store.rs".into(),
            line_start: 100,
            line_end: 120,
            visibility: None,
        };
        assert!(!is_test_symbol(&s));
        // "latest" must not trip the substring trap.
        let s2 = Symbol {
            parent: Some("latest".into()),
            ..s
        };
        assert!(!is_test_symbol(&s2));
    }

    #[test]
    fn runner_detection_prefers_cargo() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname='x'").unwrap();
        assert_eq!(detect_runner(dir.path()).as_deref(), Some("cargo"));
    }

    #[test]
    fn cargo_command_filters_by_test_name() {
        let tests = vec![
            AffectedTest {
                name: "unit_test".into(),
                file: "src/widget.rs".into(),
                line: 12,
                kind: SymbolKind::Function,
                via_depth: 1,
            },
            AffectedTest {
                name: "integ_test".into(),
                file: "tests/it.rs".into(),
                line: 1,
                kind: SymbolKind::Function,
                via_depth: 1,
            },
        ];
        let cmd = build_command(Some("cargo"), &tests).unwrap();
        assert!(cmd.starts_with("cargo test -- "), "{cmd}");
        assert!(
            cmd.contains("unit_test") && cmd.contains("integ_test"),
            "{cmd}"
        );
        // No tests -> no command.
        assert!(build_command(Some("cargo"), &[]).is_none());
    }
}
