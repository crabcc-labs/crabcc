//! Lightweight `git diff` shell-out used by the `--since SHA` query filter.
//!
//! Returns the set of repo-relative file paths that changed between
//! `<since>` and the current `HEAD`. We deliberately shell out to the
//! user's `git` binary rather than depending on libgit2:
//!
//! - `git` is already a hard requirement of the workspace (walker.rs uses
//!   `.gitignore` semantics and the repo layout assumes a working tree).
//! - libgit2 / `git2` would add ~2 MB of compiled code and a second
//!   gitignore implementation; our needs here (one diff invocation) don't
//!   justify that cost.
//! - Output is the exact format `git diff --name-only` already produces,
//!   so we don't need to mediate between APIs.

use anyhow::{anyhow, Context, Result};
use ahash::HashSet;
use std::path::Path;
use std::process::Command;

/// Files that changed in the working tree between `<since>` and `HEAD`,
/// as repo-relative paths.
///
/// `since` accepts anything `git diff` accepts: a SHA prefix
/// (`abc1234`), a ref name (`origin/main`), or a relative ref
/// (`HEAD~5`, `HEAD@{1.day.ago}`). We don't validate the input — we
/// hand it to `git` and surface its error message verbatim.
///
/// Includes added, modified, and renamed files (the new path side).
/// Removed files are intentionally excluded — a query filter that
/// included them would only ever produce zero hits since the index
/// has already dropped their rows.
pub fn changed_files_since(root: &Path, since: &str) -> Result<HashSet<String>> {
    // `--name-only` prints one path per line, repo-relative.
    // `--diff-filter=AMR` keeps Added / Modified / Renamed; drops Deleted.
    // `<since>...HEAD` (three dots) shows changes on HEAD's side of the
    // merge base — that's "changed since this point" for branched work.
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--name-only", "--diff-filter=AMR"])
        .arg(format!("{since}...HEAD"))
        .output()
        .with_context(|| format!("invoking `git diff` against {since:?}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!(
            "git diff --name-only {since}...HEAD failed: {}",
            stderr.trim()
        ));
    }

    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::write;
    use std::process::Command as Cmd;

    fn init_repo(dir: &Path) {
        Cmd::new("git")
            .args(["-C", &dir.display().to_string(), "init", "-q"])
            .status()
            .unwrap();
        Cmd::new("git")
            .args([
                "-C",
                &dir.display().to_string(),
                "config",
                "user.email",
                "t@t",
            ])
            .status()
            .unwrap();
        Cmd::new("git")
            .args(["-C", &dir.display().to_string(), "config", "user.name", "t"])
            .status()
            .unwrap();
        Cmd::new("git")
            .args([
                "-C",
                &dir.display().to_string(),
                "config",
                "commit.gpgsign",
                "false",
            ])
            .status()
            .unwrap();
    }

    fn commit_all(dir: &Path, msg: &str) {
        Cmd::new("git")
            .args(["-C", &dir.display().to_string(), "add", "-A"])
            .status()
            .unwrap();
        Cmd::new("git")
            .args(["-C", &dir.display().to_string(), "commit", "-q", "-m", msg])
            .status()
            .unwrap();
    }

    #[test]
    #[ignore = "spawns git in tempdir; flake-prone on bare CI containers — run locally with --ignored"]
    fn changed_files_since_finds_added_and_modified() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        write(root.join("a.ts"), "export const a = 1;").unwrap();
        write(root.join("untouched.ts"), "export const u = 1;").unwrap();
        commit_all(root, "init");

        // Capture HEAD-1 as the "since" anchor.
        let head1 = String::from_utf8(
            Cmd::new("git")
                .args(["-C", &root.display().to_string(), "rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        // Modify one file, add another, leave the third alone.
        write(root.join("a.ts"), "export const a = 2;").unwrap();
        write(root.join("b.ts"), "export const b = 2;").unwrap();
        commit_all(root, "round 2");

        let changed = changed_files_since(root, &head1).unwrap();
        assert!(changed.contains("a.ts"), "expected a.ts in {changed:?}");
        assert!(changed.contains("b.ts"), "expected b.ts in {changed:?}");
        assert!(
            !changed.contains("untouched.ts"),
            "expected untouched.ts NOT in {changed:?}"
        );
    }

    #[test]
    #[ignore = "spawns git in tempdir; flake-prone on bare CI containers — run locally with --ignored"]
    fn changed_files_since_empty_when_no_diff() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        write(root.join("a.ts"), "x").unwrap();
        commit_all(root, "init");

        // Diffing HEAD against itself yields no files.
        let changed = changed_files_since(root, "HEAD").unwrap();
        assert!(changed.is_empty(), "expected empty, got: {changed:?}");
    }

    #[test]
    #[ignore = "spawns git in tempdir; flake-prone on bare CI containers — run locally with --ignored"]
    fn changed_files_since_errors_on_bad_revision() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        write(root.join("a.ts"), "x").unwrap();
        commit_all(root, "init");

        // Garbage revision → git exits non-zero, helper surfaces the error.
        let err = changed_files_since(root, "definitely-not-a-ref").unwrap_err();
        assert!(
            err.to_string().contains("definitely-not-a-ref")
                || err.to_string().contains("unknown")
                || err.to_string().to_lowercase().contains("ambiguous"),
            "expected error to mention bad revision, got: {err}"
        );
    }
}
