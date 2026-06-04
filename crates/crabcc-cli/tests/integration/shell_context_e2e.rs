//! Core e2e checks for the SessionStart context injector
//! (`crabcc shell context`): on-by-default + disable switch, and that an
//! indexed repo yields the smart (index-derived) context.

use std::path::Path;
use std::process::Command;

fn crabcc() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_crabcc"));
    c.env_remove("CRABCC_HOME");
    c.env_remove("CRABCC_NO_CTX_INJECT");
    c.env("CRABCC_BACKUP_DISABLE", "1");
    c.env("CRABCC_NO_HINT", "1");
    c.env("CRABCC_NO_DEPRECATION_WARN", "1");
    c
}

fn additional_context(out: &[u8]) -> String {
    let v: serde_json::Value = serde_json::from_slice(out)
        .unwrap_or_else(|e| panic!("expected JSON: {e}; got {:?}", String::from_utf8_lossy(out)));
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext")
        .to_string()
}

#[test]
fn on_by_default_with_disable_switch() {
    // Default: emits a valid SessionStart envelope (static fallback when
    // no index), mentioning context7.
    let on = crabcc().args(["shell", "context"]).output().unwrap();
    assert!(on.status.success());
    let ctx = additional_context(&on.stdout);
    assert!(ctx.contains("context7"), "missing context7 nudge: {ctx}");

    // CRABCC_NO_CTX_INJECT=1 suppresses it entirely.
    let off = crabcc()
        .env("CRABCC_NO_CTX_INJECT", "1")
        .args(["shell", "context"])
        .output()
        .unwrap();
    assert!(off.status.success());
    assert!(
        off.stdout.is_empty(),
        "CRABCC_NO_CTX_INJECT=1 must suppress injection; got: {}",
        String::from_utf8_lossy(&off.stdout)
    );
}

#[test]
fn indexed_repo_yields_smart_context() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        Path::new(project.path()).join("lib.rs"),
        "pub struct Greeter { pub name: String }\n\
         pub fn make() -> Greeter { Greeter { name: String::new() } }\n\
         pub fn a(g: &Greeter) -> usize { g.name.len() }\n\
         pub fn b(g: &Greeter) -> usize { g.name.len() }\n",
    )
    .unwrap();
    let idx = crabcc()
        .env("CRABCC_HOME", home.path())
        .args(["index", "--root"])
        .arg(project.path())
        .output()
        .unwrap();
    assert!(idx.status.success());

    let out = crabcc()
        .env("CRABCC_HOME", home.path())
        .args(["shell", "context", "--root"])
        .arg(project.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let ctx = additional_context(&out.stdout);
    // The smart path (only reachable with an index) announces itself and
    // points at crabcc lookup.
    assert!(
        ctx.contains("indexed this repo"),
        "expected smart index-derived context, got: {ctx}"
    );
}
