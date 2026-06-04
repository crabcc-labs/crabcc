//! End-to-end checks for the PreToolUse Bash-rewrite path
//! (`crabcc shell rewrite`). The pure planner is unit-tested in
//! `src/shell_rewrite.rs`; these tests exercise the *binary*: the JSON
//! envelope it prints, that the rewritten command is a real runnable
//! crabcc subcommand, that non-search commands pass through, that the
//! kill-switch works, and that the symbol upgrade actually reduces
//! output vs the grep it replaces.
//!
//! Regression guard: the rewriter once emitted `crabcc refs IDENT`,
//! which is not a real subcommand (`refs` lives under `crabcc lookup`).
//! `symbol_upgrade_emits_runnable_command` fails on that bug because the
//! emitted command exits non-zero / the envelope lacks `lookup`.

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

/// Fixture with a clean symbol (`Greeter`) plus a symbol deliberately
/// buried in comment noise (`Widget`) so the symbol upgrade has textual
/// matches to filter out.
fn write_fixture(root: &Path) {
    let mut noisy = String::new();
    for i in 0..40 {
        noisy.push_str(&format!("// line {i}: Widget mentioned here in a comment\n"));
    }
    noisy.push_str("pub struct Widget { pub id: u32 }\n");
    noisy.push_str("pub fn use_widget(w: &Widget) -> u32 { w.id }\n");
    std::fs::write(root.join("noisy.rs"), noisy).unwrap();
    std::fs::write(
        root.join("lib.rs"),
        "pub struct Greeter { pub name: String }\n\
         pub fn greet(g: &Greeter) -> String { g.name.clone() }\n",
    )
    .unwrap();
}

/// (project, crabcc_home) with the fixture indexed so symbol lookups
/// resolve.
fn indexed_project() -> (tempfile::TempDir, tempfile::TempDir) {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_fixture(project.path());
    let out = crabcc()
        .env("CRABCC_HOME", home.path())
        .args(["index", "--root"])
        .arg(project.path())
        .output()
        .expect("crabcc index");
    assert!(
        out.status.success(),
        "fixture index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (project, home)
}

/// Run `crabcc shell rewrite` and return the rewritten command string
/// from the PreToolUse envelope, or `None` on passthrough (empty stdout).
fn rewrite(project: &Path, home: &Path, command: &str) -> Option<String> {
    let out = crabcc()
        .env("CRABCC_HOME", home)
        .args(["shell", "rewrite", "--root"])
        .arg(project)
        .args(["--command", command])
        .output()
        .expect("crabcc shell rewrite");
    assert!(
        out.status.success(),
        "shell rewrite must always exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    if stdout.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("rewrite output must be valid JSON");
    Some(
        v["hookSpecificOutput"]["updatedInput"]["command"]
            .as_str()
            .expect("envelope must carry updatedInput.command")
            .to_string(),
    )
}

#[test]
fn symbol_upgrade_emits_runnable_command() {
    let (project, home) = indexed_project();

    let wrapped = rewrite(project.path(), home.path(), "grep -rn Greeter .")
        .expect("grep for an indexed symbol should rewrite");

    // The emitted command must target the real subcommand surface.
    assert!(
        wrapped.contains("crabcc lookup refs Greeter"),
        "expected `crabcc lookup refs Greeter`, got: {wrapped}"
    );
    // And it must carry the disclosing header + rg fallback.
    assert!(
        wrapped.contains("crabcc-rewrite") && wrapped.contains("rg Greeter"),
        "missing provenance header / rg fallback: {wrapped}"
    );

    // Regression guard: run the emitted subcommand for real. With the
    // old `crabcc refs` bug this exits non-zero ("unrecognized
    // subcommand"); `crabcc lookup refs` succeeds and returns refs.
    let out = crabcc()
        .env("CRABCC_HOME", home.path())
        .args(["lookup", "refs", "Greeter", "--root"])
        .arg(project.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "emitted command is not a valid crabcc subcommand: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.stdout.is_empty() && out.stdout != b"[]\n",
        "emitted command returned no refs (silent failure): {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn find_swaps_to_ripgrep() {
    let (project, home) = indexed_project();
    let wrapped = rewrite(project.path(), home.path(), "find . -name '*.rs'")
        .expect("find -name should rewrite");
    assert!(
        wrapped.contains("rg --files -g '*.rs'"),
        "expected ripgrep --files swap, got: {wrapped}"
    );
}

#[test]
fn non_search_command_passes_through() {
    let (project, home) = indexed_project();
    assert!(
        rewrite(project.path(), home.path(), "ls -la").is_none(),
        "non-search command must pass through (empty stdout)"
    );
    // Pipes / metacharacters must also pass through unchanged.
    assert!(
        rewrite(project.path(), home.path(), "grep -rn Greeter . | head").is_none(),
        "piped command must pass through"
    );
}

#[test]
fn no_rewrite_env_disables() {
    let (project, home) = indexed_project();
    let out = crabcc()
        .env("CRABCC_HOME", home.path())
        .env("CRABCC_NO_REWRITE", "1")
        .args(["shell", "rewrite", "--root"])
        .arg(project.path())
        .args(["--command", "grep -rn Greeter ."])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "CRABCC_NO_REWRITE=1 must suppress all rewrites; got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn symbol_upgrade_reduces_output_vs_grep() {
    let (project, home) = indexed_project();

    // What the agent's raw grep would have emitted (40 comment hits +
    // the real code), run in the project dir.
    let grep = Command::new("grep")
        .args(["-rn", "Widget", "."])
        .current_dir(project.path())
        .output()
        .expect("system grep");
    let grep_bytes = grep.stdout.len();
    assert!(grep_bytes > 0, "fixture grep produced nothing");

    // What the rewrite runs instead.
    let refs = crabcc()
        .env("CRABCC_HOME", home.path())
        .args(["lookup", "refs", "Widget", "--root"])
        .arg(project.path())
        .output()
        .unwrap();
    assert!(refs.status.success());
    let refs_bytes = refs.stdout.len();

    assert!(
        refs_bytes < grep_bytes,
        "symbol upgrade did not reduce output: grep={grep_bytes}B vs refs={refs_bytes}B"
    );
}
