//! Path resolution + legacy db migration + git-root walk-up.
//!
//! `resolve_db_path` is the public API — every Palace open routes through
//! it. `find_git_root` is the standalone walk-up used by [`PalaceRegistry`]
//! and any caller that wants to canonicalize a `cwd` before invoking the
//! registry.
//!
//! Other helpers (`sanitize_slug`, `repo_storage_key`, `git_origin_url`,
//! `crabcc_home_dir`) are `pub(super)` so the parent module's tests can
//! exercise them directly.

use anyhow::{Context, Result};
use crabcc_core::hash::sha256_hex;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve the on-disk path for a repo's `memory.db`. Layout:
///
/// ```text
/// $CRABCC_HOME/repos/<slug>-<hash6>/memory.db
/// ```
///
/// * `$CRABCC_HOME` defaults to `$HOME/.crabcc` (matches the convention
///   already used by `crabcc-cli/backup.rs`, `model_info`, agents/, etc.).
/// * `<slug>` is the repo dir's basename, ascii-lowered, with non-
///   `[a-z0-9_-]` collapsed to `-`. Falls back to `unknown-repo` when
///   the path has no terminal component.
/// * `<hash6>` is the first 6 hex chars of `sha256(origin_url)`. When
///   no `remote.origin.url` is set (fresh `git init`, non-git dir), the
///   slug stands alone — at the cost of basename collisions across
///   unrelated repos sharing a name.
///
/// Centralising memory.db in `$CRABCC_HOME` means worktrees of the same
/// repo share one db (they share `.git/config`), and `git clean -fdx`
/// in the working tree doesn't blow it away.
pub fn resolve_db_path(repo_root: &Path) -> Result<PathBuf> {
    let home = crabcc_home_dir()?;
    let key = repo_storage_key(repo_root);
    Ok(home.join("repos").join(key).join("memory.db"))
}

pub(super) fn crabcc_home_dir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CRABCC_HOME") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME").context("$HOME is not set")?;
    Ok(PathBuf::from(home).join(".crabcc"))
}

pub(super) fn repo_storage_key(repo_root: &Path) -> String {
    let raw_basename = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown-repo".into());
    let slug = sanitize_slug(&raw_basename);
    match git_origin_url(repo_root) {
        Some(url) => {
            let hash6: String = sha256_hex(url.as_bytes()).chars().take(6).collect();
            format!("{slug}-{hash6}")
        }
        None => slug,
    }
}

pub(super) fn sanitize_slug(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| {
            let lower = c.to_ascii_lowercase();
            if lower.is_ascii_alphanumeric() || lower == '-' || lower == '_' {
                lower
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = s.trim_matches('-');
    if trimmed.is_empty() {
        "unknown-repo".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(super) fn git_origin_url(repo_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// One-shot migration: if `<repo_root>/.crabcc/memory.db` exists and
/// the new global path doesn't, copy it over so the user's existing
/// drawers carry forward. Idempotent — once the new path exists,
/// subsequent opens skip the check.
pub(super) fn migrate_legacy_if_needed(repo_root: &Path, new_path: &Path) -> Result<()> {
    let legacy = repo_root.join(".crabcc").join("memory.db");
    if !legacy.exists() || new_path.exists() {
        return Ok(());
    }
    if let Some(parent) = new_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::copy(&legacy, new_path)
        .with_context(|| format!("copy {} -> {}", legacy.display(), new_path.display()))?;
    eprintln!(
        "crabcc-memory: migrated drawers from {} -> {}",
        legacy.display(),
        new_path.display()
    );
    Ok(())
}

/// Walk up from `start` looking for `.git/`. Returns the first ancestor
/// containing one, or `None` if not in a git repo.
///
/// Hot paths (the MCP tool dispatch loop) should prefer routing through
/// [`super::PalaceRegistry::open_for`] / [`super::PalaceRegistry::resolve_git_root`],
/// which memoize this walk so a flurry of calls with the same `cwd` only
/// pays the canonicalize + ancestor scan once per minute.
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut p = start.canonicalize().ok()?;
    loop {
        if p.join(".git").exists() {
            return Some(p);
        }
        p = p.parent()?.to_path_buf();
    }
}
