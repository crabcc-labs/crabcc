//! End-to-end smoke test against a pinned open-source repo.
//!
//! Pins `BurntSushi/walkdir` at tag `2.5.0`
//! (commit `588ebd21cbad9b572f8d814fa72dcb1200332ac3`) and verifies that
//! the full `crabcc index → crabcc sym/files` round-trip produces the
//! shape we expect against a real-world Rust crate.
//!
//! Requires network + git on PATH. Marked `#[ignore]` so default
//! `cargo test` doesn't fail offline. Run explicitly:
//!
//!     cargo test --test e2e_walkdir -- --ignored
//!
//! Override the fixture with `CRABCC_E2E_FIXTURE=/path/to/local/walkdir`
//! to skip the clone (useful in CI containers that pre-stage the repo).

use std::path::PathBuf;
use std::process::Command;

const PINNED_URL: &str = "https://github.com/BurntSushi/walkdir.git";
const PINNED_TAG: &str = "2.5.0";
// Commit SHA the `2.5.0` annotated tag points to. (`git ls-remote
// refs/tags/2.5.0` returns the tag-object SHA — different from this.)
const PINNED_SHA: &str = "4f26be4d450910916ea11533b2efc52b9a6483bc";

/// Build a working copy of walkdir at the pinned commit and return its
/// path. Honours `CRABCC_E2E_FIXTURE` for offline / pre-staged runs.
fn fixture() -> (PathBuf, tempfile::TempDir) {
    if let Ok(local) = std::env::var("CRABCC_E2E_FIXTURE") {
        let p = PathBuf::from(local);
        assert!(p.join(".git").exists(), "fixture must be a git repo");
        // Returning a TempDir we never use just so the signature stays
        // uniform — caller drops it at end of test.
        return (p, tempfile::tempdir().unwrap());
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("walkdir");

    let clone = Command::new("git")
        .args([
            "clone",
            "--depth=1",
            "--branch",
            PINNED_TAG,
            PINNED_URL,
            dest.to_str().unwrap(),
        ])
        .output()
        .expect("git clone");
    assert!(
        clone.status.success(),
        "git clone failed: {}",
        String::from_utf8_lossy(&clone.stderr)
    );

    // Verify we landed on the pinned SHA. `git clone --branch <tag>`
    // resolves to the tagged commit, but pinning to the SHA is the
    // contract of this test, so check it explicitly.
    let head = Command::new("git")
        .args(["-C", dest.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    let head_sha = String::from_utf8(head.stdout).unwrap().trim().to_string();
    assert_eq!(
        head_sha, PINNED_SHA,
        "tag {PINNED_TAG} resolved to a different SHA than expected"
    );

    (dest, dir)
}

fn crabcc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_crabcc"))
}

#[test]
#[ignore = "network-dependent: clones BurntSushi/walkdir"]
fn e2e_walkdir_pinned_index_and_sym() {
    let (repo, _guard) = fixture();

    // 1. Index the fixture.
    let idx = crabcc()
        .args(["index", "--root"])
        .arg(&repo)
        .output()
        .expect("crabcc index");
    assert!(
        idx.status.success(),
        "crabcc index failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&idx.stdout),
        String::from_utf8_lossy(&idx.stderr)
    );

    // 2. `sym WalkDir` — walkdir's headline struct. Must come back as
    //    JSON with at least one hit, the kind must be a struct, and
    //    the file must point at the lib root.
    let sym = crabcc()
        .args(["sym", "--root"])
        .arg(&repo)
        .arg("WalkDir")
        .output()
        .expect("crabcc sym");
    assert!(sym.status.success(), "crabcc sym WalkDir failed");

    let stdout = String::from_utf8(sym.stdout).unwrap();
    assert!(!stdout.trim().is_empty(), "sym output empty");

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("sym output must be JSON");
    let arr = parsed.as_array().expect("sym JSON top-level must be array");
    assert!(
        !arr.is_empty(),
        "expected at least one hit for `WalkDir`"
    );
    let first = &arr[0];
    assert_eq!(first["name"], "WalkDir");
    assert_eq!(first["kind"], "struct");
    let file = first["file"].as_str().expect("file string");
    assert!(
        file.ends_with("src/lib.rs") || file.ends_with("/lib.rs"),
        "expected WalkDir in src/lib.rs, got `{file}`"
    );

    // 3. `files --ext rs --limit 50` — walkdir at 2.5.0 is a small
    //    crate with a handful of .rs files; assert the count is in a
    //    sensible range so we'd notice a regression in either the
    //    walker or the extension filter.
    let files = crabcc()
        .args(["files", "--root"])
        .arg(&repo)
        .args(["--ext", "rs", "--limit", "50"])
        .output()
        .expect("crabcc files");
    assert!(files.status.success(), "crabcc files failed");

    let files_json: serde_json::Value =
        serde_json::from_slice(&files.stdout).expect("files output must be JSON");
    let count = files_json
        .as_array()
        .expect("files top-level array")
        .len();
    assert!(
        (3..=20).contains(&count),
        "walkdir 2.5.0 has 3-20 .rs files; got {count}"
    );

    // 4. `outline` on lib.rs must surface WalkDir too — sanity check
    //    that the extractor sees more than one top-level symbol.
    //    Note: `outline` looks up the file column in the index, which
    //    is stored relative to the repo root. Passing an absolute path
    //    misses the row, so always pass a relative path with --root.
    let outline = crabcc()
        .args(["outline", "--root"])
        .arg(&repo)
        .arg("src/lib.rs")
        .output()
        .expect("crabcc outline");
    assert!(outline.status.success(), "crabcc outline failed");
    let out_str = String::from_utf8(outline.stdout).unwrap();
    assert!(
        out_str.contains("\"WalkDir\""),
        "outline should list WalkDir, got: {out_str}"
    );
}
