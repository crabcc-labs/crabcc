# Task 15 — Integration test: index → why → blast-radius → hot-symbols

## Context

Wave 3, parallel. The v4 data-layer rewrite (Tasks 1–14) needs an
end-to-end smoke test that proves the CLI surface actually works against a
freshly-indexed tiny project. This gates the merge: success criterion S5
("`cargo test --workspace` 100% green") is the merge gate, and this test
is the visible-from-the-user-side leg of it.

Two files to touch:

1. `crates/crabcc-cli/tests/integration/graph_v4.rs` — new file, ~150
   LoC. Drives the `crabcc` binary via `Command::new(env!("CARGO_BIN_EXE_
   crabcc"))` against a temp project containing three known-related Rust
   functions.
2. `crates/crabcc-cli/tests/integration/mod.rs` — one-line append to
   register the new module.

The existing per-test harness in `crates/crabcc-cli/tests/integration/
auto_index.rs` is the reference style: small `crabcc()` builder that
clears `CRABCC_HOME`, sets `CRABCC_BACKUP_DISABLE=1` /
`CRABCC_NO_HINT=1` / `CRABCC_NO_DEPRECATION_WARN=1`, and is overridden
per-test with a fresh tempdir.

## What to change

### File 1: `crates/crabcc-cli/tests/integration/graph_v4.rs` (new)

Create the file with these exact contents:

```rust
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
```

### File 2: `crates/crabcc-cli/tests/integration/mod.rs`

The file currently looks like this (4 directives, lines 15–20):

```rust
#[path = "integration/agent_dry_run.rs"]
mod agent_dry_run;
#[path = "integration/auto_index.rs"]
mod auto_index;
#[path = "integration/e2e_walkdir.rs"]
mod e2e_walkdir;
```

Wait — `mod.rs` itself lives under `tests/integration/`, while the `mod`
declarations above live in `tests/integration.rs` (the root binary).
**Re-check before editing.** The allow-list for this task is
`crates/crabcc-cli/tests/integration/mod.rs`, which does not currently
exist in the repo (today's test layout uses `#[path = ...]` declarations
in `tests/integration.rs` and a sibling `integration/` directory with no
`mod.rs`).

There are two paths forward; pick whichever matches what's on disk **at
the moment this task runs**:

1. **If `crates/crabcc-cli/tests/integration/mod.rs` exists**, append
   exactly one line to it:

   ```rust
   pub mod graph_v4;
   ```

2. **If `crates/crabcc-cli/tests/integration/mod.rs` does not exist**,
   the orchestrator's allow-list for this task is mis-scoped and the
   actual integration entry-point is `crates/crabcc-cli/tests/
   integration.rs` (which IS NOT in your allow-list). In that case:
   - Create `crates/crabcc-cli/tests/integration/mod.rs` with this
     single line:

     ```rust
     pub mod graph_v4;
     ```

   - **Do not** modify `crates/crabcc-cli/tests/integration.rs`. The
     integration-test binary will only auto-pick up `graph_v4` if
     `integration.rs` adds the `#[path = "integration/graph_v4.rs"]
     mod graph_v4;` directive; that wiring is intentionally deferred
     to a follow-up because `integration.rs` is outside this task's
     allow-list. Note this in the commit body or PR description so the
     human reviewer knows to add the directive before merge.

Run this check first (in the worktree root) before deciding which branch
to take:

```bash
test -f crates/crabcc-cli/tests/integration/mod.rs && echo EXISTS || echo MISSING
```

## Definition of done

- `crates/crabcc-cli/tests/integration/graph_v4.rs` exists with the
  contents shown above (verbatim, ~155 LoC).
- `crates/crabcc-cli/tests/integration/mod.rs` either contains the
  single line `pub mod graph_v4;` (if it was created in branch 2) or has
  that line appended (branch 1).
- No other file in the workspace is touched. Specifically:
  `crates/crabcc-cli/tests/integration.rs` (the binary root) is NOT
  modified, because it is not in this task's allow-list.

Do not run `cargo build`, `cargo test`, or any other build or test command.

Do not modify any other file. Do not invent extra files.

Then commit with this exact message:

    test(graph): integration — index → why → blast-radius → hot-symbols end-to-end
