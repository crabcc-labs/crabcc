//! End-to-end test for `crabcc affected`: index a tiny Rust project with a
//! known call shape, then assert `affected --symbol core_fn` selects exactly
//! the tests that transitively call `core_fn` and emits a cargo command.
//!
//! Fixture (flat files = modules, mirroring `graph_v4.rs`):
//!   core.rs    `core_fn`   — the changed symbol
//!   widget.rs  `widget` calls core_fn (NON-test caller) + `#[cfg(test)] mod
//!              tests { fn unit_uses_core }` calls core_fn (unit test)
//!   tests/it.rs `integ_uses_core` calls core_fn (integration test, under tests/)
//! `Cargo.toml` present so runner detection returns `cargo`.
//!
//! Expected: tests = {unit_uses_core, integ_uses_core}; `widget` excluded.

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

fn write_fixture(root: &Path) {
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"fx\"\n").unwrap();
    std::fs::write(root.join("core.rs"), "pub fn core_fn() -> i32 { 1 }\n").unwrap();
    // Call core_fn DIRECTLY (a bare call expr). crabcc's extractor records the
    // outermost call only, so wrapping it in `assert_eq!(core_fn(), 1)` would
    // link the test to the `assert_eq` macro, not to core_fn.
    std::fs::write(
        root.join("widget.rs"),
        "pub fn widget() -> i32 { crate::core::core_fn() }\n\
         #[cfg(test)]\n\
         mod tests {\n\
         \x20   #[test]\n\
         \x20   fn unit_uses_core() { let _ = crate::core::core_fn(); }\n\
         }\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(
        root.join("tests/it.rs"),
        "#[test]\nfn integ_uses_core() { let _ = crate::core::core_fn(); }\n",
    )
    .unwrap();
}

fn run_index(project: &Path, home: &Path) {
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
}

fn run_affected(project: &Path, home: &Path, args: &[&str]) -> serde_json::Value {
    let out = crabcc()
        .env("CRABCC_HOME", home)
        .args(["affected"])
        .args(args)
        .args(["--root"])
        .arg(project)
        .output()
        .expect("crabcc affected");
    assert!(
        out.status.success(),
        "affected {args:?} failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "expected JSON from `affected {args:?}`, got: {}\n(parse error: {e})",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn test_names(v: &serde_json::Value) -> Vec<String> {
    v.get("tests")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn affected_selects_transitive_tests_and_emits_cargo_command() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_fixture(project.path());
    run_index(project.path(), home.path());

    let v = run_affected(project.path(), home.path(), &["--symbol", "core_fn"]);
    let names = test_names(&v);

    assert!(
        names.iter().any(|n| n == "unit_uses_core"),
        "unit test (mod tests) should be selected; got {names:?} body={v}",
    );
    assert!(
        names.iter().any(|n| n == "integ_uses_core"),
        "integration test (under tests/) should be selected; got {names:?} body={v}",
    );
    assert!(
        !names.iter().any(|n| n == "widget"),
        "plain caller `widget` must NOT be selected; got {names:?} body={v}",
    );

    assert_eq!(
        v.get("runner").and_then(|r| r.as_str()),
        Some("cargo"),
        "runner should be cargo (Cargo.toml present); body={v}",
    );
    let cmd = v
        .get("command")
        .and_then(|c| c.as_str())
        .unwrap_or_default();
    assert!(
        cmd.starts_with("cargo test -- "),
        "command should be a cargo filter; got {cmd:?} body={v}",
    );
}
