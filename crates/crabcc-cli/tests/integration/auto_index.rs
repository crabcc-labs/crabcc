//! Auto-indexing on first read + centralised `$CRABCC_HOME/repos/<key>/`
//! layout. Built per `feat/auto-index-and-url-root` — see
//! `crates/crabcc-cli/src/root_resolver.rs` and `auto_index.rs`.
//!
//! Coverage matrix:
//!   * outline on fresh project → auto-indexes + returns symbols + warns
//!   * stderr warning text is exactly the contract callers depend on
//!   * `CRABCC_NO_AUTO_INDEX=1` opts out (returns empty, no warning)
//!   * second invocation is a no-op (no warning, same output)
//!   * `sym` / `files` / `fuzzy` all auto-index (regression: only outline)
//!   * In-repo `.crabcc/` wins when present (artifacts go there, not home)
//!   * Centralised layout is keyed by canonicalised path (stable across runs)
//!   * `index/*` and `memory/*` paths skip the auto-index guard (no warning)

use std::path::{Path, PathBuf};
use std::process::Command;

fn crabcc() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_crabcc"));
    // Each test gets its own crabcc_home → no pollution of ~/.crabcc.
    // Caller MUST override with `.env("CRABCC_HOME", ...)` afterwards.
    c.env_remove("CRABCC_HOME");
    // Skip the per-repo backup snapshot — it tries to copy a (mostly
    // empty) .crabcc/ tree and noisies up the test stderr.
    c.env("CRABCC_BACKUP_DISABLE", "1");
    // Stable hint behaviour regardless of where the test runs.
    c.env("CRABCC_NO_HINT", "1");
    c.env("CRABCC_NO_DEPRECATION_WARN", "1");
    c
}

/// Minimal Rust-ish source tree: enough symbols for outline / sym /
/// files / fuzzy to all return non-empty.
fn write_fixture(root: &Path) {
    std::fs::write(
        root.join("lib.rs"),
        r#"
pub struct Greeter { pub name: String }
impl Greeter {
    pub fn greet(&self) -> String { format!("hello {}", self.name) }
    pub fn bye(&self) -> &'static str { "bye" }
}
pub fn alpha() -> u32 { 1 }
pub fn omega() -> u32 { 2 }
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("util.rs"),
        r#"
pub fn helper() -> bool { true }
pub struct Config { pub debug: bool }
"#,
    )
    .unwrap();
}

fn fresh_project() -> (tempfile::TempDir, tempfile::TempDir) {
    let project = tempfile::tempdir().unwrap();
    let crabcc_home = tempfile::tempdir().unwrap();
    write_fixture(project.path());
    (project, crabcc_home)
}

fn json(out: &[u8]) -> serde_json::Value {
    serde_json::from_slice(out).unwrap_or_else(|e| {
        panic!(
            "expected JSON, got: {}\n(parse error: {e})",
            String::from_utf8_lossy(out)
        )
    })
}

#[test]
fn outline_on_fresh_project_auto_indexes_and_warns() {
    let (project, home) = fresh_project();

    let out = crabcc()
        .env("CRABCC_HOME", home.path())
        .args(["outline", "--root"])
        .arg(project.path())
        .arg("lib.rs")
        .output()
        .expect("crabcc outline");

    assert!(
        out.status.success(),
        "outline failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("project not indexed yet"),
        "expected auto-index warning in stderr; got:\n{stderr}"
    );
    assert!(
        stderr.contains("indexed ") && stderr.contains(" files"),
        "expected post-index summary; got:\n{stderr}"
    );

    let arr = json(&out.stdout);
    let arr = arr.as_array().expect("outline JSON must be an array");
    assert!(!arr.is_empty(), "outline returned empty after auto-index");
    let names: Vec<&str> = arr.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(names.contains(&"Greeter"), "missing Greeter; got {names:?}");
    assert!(names.contains(&"alpha"), "missing alpha; got {names:?}");
}

#[test]
fn second_invocation_is_silent_noop() {
    let (project, home) = fresh_project();

    // Prime the index.
    let _ = crabcc()
        .env("CRABCC_HOME", home.path())
        .args(["outline", "--root"])
        .arg(project.path())
        .arg("lib.rs")
        .output()
        .unwrap();

    // Second call must NOT re-index (no warning) and must return the
    // same output.
    let out = crabcc()
        .env("CRABCC_HOME", home.path())
        .args(["outline", "--root"])
        .arg(project.path())
        .arg("lib.rs")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("project not indexed yet"),
        "auto-index re-fired on a populated store; stderr={stderr}"
    );
    let arr = json(&out.stdout);
    assert!(!arr.as_array().unwrap().is_empty());
}

#[test]
fn no_auto_index_env_opts_out() {
    let (project, home) = fresh_project();

    let out = crabcc()
        .env("CRABCC_HOME", home.path())
        .env("CRABCC_NO_AUTO_INDEX", "1")
        .args(["outline", "--root"])
        .arg(project.path())
        .arg("lib.rs")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("project not indexed yet"),
        "auto-index ran despite opt-out; stderr={stderr}"
    );
    // Output must be valid JSON, even if empty.
    let arr = json(&out.stdout);
    assert!(
        arr.as_array().unwrap().is_empty(),
        "expected empty outline with opt-out; got {arr:?}"
    );
}

#[test]
fn sym_files_fuzzy_all_auto_index() {
    // Each of these read-side commands must trigger the guard. We use
    // a fresh crabcc_home per command so they each see a virgin store.
    for verb in ["sym", "files", "fuzzy"] {
        let (project, home) = fresh_project();
        let mut cmd = crabcc();
        cmd.env("CRABCC_HOME", home.path())
            .arg(verb)
            .arg("--root")
            .arg(project.path());
        match verb {
            "sym" | "fuzzy" => {
                cmd.arg("Greeter");
            }
            _ => {}
        }
        let out = cmd
            .output()
            .unwrap_or_else(|e| panic!("crabcc {verb}: {e}"));
        assert!(
            out.status.success(),
            "crabcc {verb} failed:\nstderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("project not indexed yet"),
            "verb `{verb}` did not trigger auto-index; stderr={stderr}"
        );
        let arr = json(&out.stdout);
        assert!(
            !arr.as_array().unwrap().is_empty(),
            "verb `{verb}` returned empty after auto-index"
        );
    }
}

#[test]
fn index_command_does_not_print_auto_index_warning() {
    // Running `crabcc index` itself shouldn't print the auto-index
    // warning — we're explicitly indexing, no need to nag.
    let (project, home) = fresh_project();
    let out = crabcc()
        .env("CRABCC_HOME", home.path())
        .args(["index", "--root"])
        .arg(project.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("project not indexed yet"),
        "auto-index warning leaked into `crabcc index` stderr: {stderr}"
    );
}

#[test]
fn in_repo_dotcrabcc_wins_when_present() {
    // Simulate a legacy project with `.crabcc/` already on disk. The
    // resolver should pick InRepo and store artifacts there, not in
    // crabcc_home/repos/<key>/.
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_fixture(project.path());
    std::fs::create_dir_all(project.path().join(".crabcc")).unwrap();

    let _ = crabcc()
        .env("CRABCC_HOME", home.path())
        .args(["outline", "--root"])
        .arg(project.path())
        .arg("lib.rs")
        .output()
        .unwrap();

    // InRepo path should now hold the SQLite index.
    assert!(
        project.path().join(".crabcc/index.db").exists(),
        "in-repo .crabcc/index.db not created"
    );
    // crabcc_home/repos/* must remain empty (or absent).
    let repos = home.path().join("repos");
    let pollution = repos.read_dir().map(|it| it.count()).unwrap_or(0);
    assert_eq!(
        pollution,
        0,
        "centralised crabcc_home was used despite InRepo fallback; {} entries in {}",
        pollution,
        repos.display()
    );
}

#[test]
fn centralised_layout_is_keyed_and_stable() {
    let (project, home) = fresh_project();
    // First run populates crabcc_home/repos/<key>/.
    let _ = crabcc()
        .env("CRABCC_HOME", home.path())
        .args(["outline", "--root"])
        .arg(project.path())
        .arg("lib.rs")
        .output()
        .unwrap();

    let repos: Vec<PathBuf> = home
        .path()
        .join("repos")
        .read_dir()
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(repos.len(), 1, "expected exactly one repo cache entry");
    let key_dir = &repos[0];
    let key = key_dir.file_name().unwrap().to_string_lossy().to_string();
    assert_eq!(
        key.len(),
        16,
        "cache key should be 16 hex chars; got `{key}`"
    );
    assert!(
        key.chars().all(|c| c.is_ascii_hexdigit()),
        "cache key not hex: `{key}`"
    );
    assert!(
        key_dir.join("index.db").exists(),
        "index.db missing under {}",
        key_dir.display()
    );

    // Second run on the same project must reuse the same key dir.
    let _ = crabcc()
        .env("CRABCC_HOME", home.path())
        .args(["outline", "--root"])
        .arg(project.path())
        .arg("lib.rs")
        .output()
        .unwrap();
    let repos2: Vec<PathBuf> = home
        .path()
        .join("repos")
        .read_dir()
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(repos2.len(), 1, "second run created a new key dir");
    assert_eq!(repos2[0], *key_dir);
}

#[test]
#[ignore = "network-dependent: clones BurntSushi/walkdir over https"]
fn url_root_clones_indexes_and_returns_outline() {
    // Pinned to the same fixture as e2e_walkdir.rs — small, stable.
    let home = tempfile::tempdir().unwrap();
    let out = crabcc()
        .env("CRABCC_HOME", home.path())
        .args([
            "outline",
            "--root",
            "https://github.com/BurntSushi/walkdir.git",
            "src/lib.rs",
        ])
        .output()
        .expect("crabcc outline (URL)");

    assert!(
        out.status.success(),
        "URL outline failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cloning") && stderr.contains("first use"),
        "expected clone warning; got: {stderr}"
    );
    assert!(stderr.contains("project not indexed yet"));

    let arr = json(&out.stdout);
    let arr = arr.as_array().unwrap();
    let names: Vec<&str> = arr.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(
        names.contains(&"WalkDir"),
        "expected WalkDir in outline; got {names:?}"
    );

    // crabcc_home/repos/<key>/source/.git must exist.
    let repos: Vec<PathBuf> = home
        .path()
        .join("repos")
        .read_dir()
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(repos.len(), 1);
    assert!(repos[0].join("source").join(".git").exists());
    assert!(repos[0].join("index.db").exists());
}

#[test]
fn invalid_local_path_errors_cleanly() {
    let out = crabcc()
        .args(["outline", "--root", "/this/path/does/not/exist/anywhere"])
        .arg("foo.rs")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected non-zero exit on bad --root"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--root") || stderr.contains("does not exist"),
        "expected actionable error; got: {stderr}"
    );
}
