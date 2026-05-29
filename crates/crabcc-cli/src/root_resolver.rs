//! Resolve `--root` to a concrete on-disk source dir + a stable layout
//! for index artifacts (DB, FTS, FSST, graph.json).
//!
//! Two flavours of layout:
//!
//! * `Layout::InRepo` — legacy `<source>/.crabcc/...`. Auto-selected when
//!   the user passes a local path AND `<path>/.crabcc/` already exists.
//!   Keeps existing repos working unchanged.
//! * `Layout::Centralised` — `$CRABCC_HOME/repos/<key>/...`. Default for
//!   URL inputs and any local path without in-repo `.crabcc/`. `<key>` is
//!   `slug-<hash6>` from `remote.origin.url` when in a git repo (so git
//!   worktrees share one index), else 16 hex of the canonical path.
//!   Force with `CRABCC_LAYOUT=centralised` (Mac dev: see `install/mac/`).
//!   `$CRABCC_HOME` defaults to `~/.crabcc`.
//!
//! URL inputs are git-cloned (shallow, blob-filtered) into
//! `<base>/source/` on first use. Subsequent invocations reuse the
//! existing clone — refresh is the user's responsibility (run
//! `git -C <source> pull` or pass `--root <local-path>`).

use anyhow::{anyhow, bail, Context, Result};
use crabcc_core::hash::sha256_hex;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolved location for a `--root` input. All artifact paths are
/// derived from `data_dir`, so swapping InRepo ↔ Centralised flips
/// every downstream path consistently.
#[derive(Debug, Clone)]
pub struct ResolvedRoot {
    /// Directory we treat as the repo root for indexing + git ops.
    pub source_dir: PathBuf,
    /// Sibling dir holding all index artifacts (db / tantivy / fsst /
    /// graph.json). For InRepo this is `<source>/.crabcc/`; for
    /// Centralised it's `$CRABCC_HOME/repos/<key>/`.
    pub data_dir: PathBuf,
    /// Layout flavour. Read by tests + diagnostics surfaces; the main
    /// dispatch only cares about `data_dir` paths.
    #[allow(dead_code)]
    pub layout: Layout,
    /// Cache key (Centralised only) — stable across invocations for the
    /// same canonical input.
    pub key: Option<String>,
    pub origin: Origin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    InRepo,
    Centralised,
}

#[derive(Debug, Clone)]
pub enum Origin {
    LocalPath(PathBuf),
    GitUrl(String),
}

impl ResolvedRoot {
    pub fn db(&self) -> PathBuf {
        self.data_dir.join("index.db")
    }
    pub fn fts_dir(&self) -> PathBuf {
        self.data_dir.join("tantivy")
    }
    pub fn graph_json(&self) -> PathBuf {
        self.data_dir.join("graph.json")
    }
    /// Human one-liner for log / warning messages. Includes the cache
    /// key for Centralised layouts so users can find the artifacts dir
    /// without re-running the resolver.
    pub fn display_origin(&self) -> String {
        match (&self.origin, &self.key) {
            (Origin::GitUrl(u), Some(k)) => format!("git: {u} [key={k}]"),
            (Origin::GitUrl(u), None) => format!("git: {u}"),
            (Origin::LocalPath(p), Some(k)) => format!("local: {} [key={k}]", p.display()),
            (Origin::LocalPath(p), None) => format!("local: {} [in-repo]", p.display()),
        }
    }
}

/// Resolve a clap-parsed `--root` value (or cwd) to a `ResolvedRoot`.
/// Side effect: may shell out to `git clone` for URL inputs and create
/// directories under `$CRABCC_HOME/repos/`.
pub fn resolve(input: Option<&Path>) -> Result<ResolvedRoot> {
    let s = match input {
        Some(p) => p.to_string_lossy().into_owned(),
        None => std::env::current_dir()?.to_string_lossy().into_owned(),
    };
    resolve_str(&s)
}

pub fn resolve_str(input: &str) -> Result<ResolvedRoot> {
    resolve_with_home(input, None)
}

/// Same as `resolve_str` but with an explicit override for the
/// `$CRABCC_HOME/repos/` directory. Tests use this to avoid mutating
/// process-global env vars (`set_var` is racy under parallel tests).
pub(crate) fn resolve_with_home(input: &str, home_override: Option<&Path>) -> Result<ResolvedRoot> {
    if is_git_url(input) {
        let (clone_url, canonical) = canonicalise_url(input);
        let key = cache_key(&canonical);
        let base = repos_dir(home_override)?.join(&key);
        let source_dir = base.join("source");
        ensure_cloned(&clone_url, &source_dir)?;
        std::fs::create_dir_all(&base)?;
        Ok(ResolvedRoot {
            source_dir,
            data_dir: base,
            layout: Layout::Centralised,
            key: Some(key),
            origin: Origin::GitUrl(clone_url),
        })
    } else {
        let raw = PathBuf::from(input);
        let abs = raw
            .canonicalize()
            .with_context(|| format!("`--root` path does not exist: {input}"))?;
        if use_in_repo_layout(&abs) {
            let in_repo = abs.join(".crabcc");
            return Ok(ResolvedRoot {
                source_dir: abs.clone(),
                data_dir: in_repo,
                layout: Layout::InRepo,
                key: None,
                origin: Origin::LocalPath(abs),
            });
        }
        let key = repo_storage_key(&abs);
        let base = repos_dir(home_override)?.join(&key);
        std::fs::create_dir_all(&base)?;
        Ok(ResolvedRoot {
            source_dir: abs.clone(),
            data_dir: base,
            layout: Layout::Centralised,
            key: Some(key),
            origin: Origin::LocalPath(abs),
        })
    }
}

fn is_git_url(s: &str) -> bool {
    s.starts_with("https://")
        || s.starts_with("http://")
        || s.starts_with("git@")
        || s.starts_with("ssh://")
        || s.starts_with("git://")
        || s.starts_with("gh:")
}

/// Returns `(clone_url, canonical_for_hash)`. `gh:owner/repo` shorthand
/// expands to `https://github.com/owner/repo`. The hash form strips
/// trailing `/` and `.git` so users get the same cache key regardless
/// of how they typed the URL.
fn canonicalise_url(s: &str) -> (String, String) {
    let expanded = if let Some(rest) = s.strip_prefix("gh:") {
        format!("https://github.com/{rest}")
    } else {
        s.to_string()
    };
    let canon = expanded
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_string();
    (expanded, canon)
}

fn cache_key(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut s = String::with_capacity(16);
    for b in digest.iter().take(8) {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// When true, index artifacts live under `<repo>/.crabcc/` (legacy).
/// Opt out on Mac / worktrees with `CRABCC_LAYOUT=centralised`.
fn use_in_repo_layout(abs: &Path) -> bool {
    if layout_forced_centralised() {
        return false;
    }
    let in_repo = abs.join(".crabcc");
    in_repo.exists() && in_repo.is_dir()
}

fn layout_forced_centralised() -> bool {
    match std::env::var("CRABCC_LAYOUT")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("centralised" | "centralized") => true,
        Some("in-repo" | "in_repo") => false,
        _ => std::env::var("CRABCC_IN_REPO_INDEX").ok().as_deref() == Some("0"),
    }
}

/// Stable per-repo key under `$CRABCC_HOME/repos/`. Matches
/// `crabcc-memory` layout so index + memory share one directory.
fn repo_storage_key(repo_root: &Path) -> String {
    let slug = sanitize_slug(
        repo_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown-repo".into())
            .as_str(),
    );
    if let Some(url) = git_origin_url(repo_root) {
        let hash6: String = sha256_hex(url.as_bytes()).chars().take(6).collect();
        return format!("{slug}-{hash6}");
    }
    if let Some(common) = git_common_dir(repo_root) {
        let hash6: String = sha256_hex(common.as_bytes()).chars().take(6).collect();
        return format!("{slug}-{hash6}");
    }
    let canon = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    cache_key(&canon.to_string_lossy())
}

fn sanitize_slug(raw: &str) -> String {
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

fn git_origin_url(repo_root: &Path) -> Option<String> {
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

fn git_common_dir(repo_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let path = PathBuf::from(&raw);
    let abs = if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    };
    abs.canonicalize()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

fn repos_dir(override_: Option<&Path>) -> Result<PathBuf> {
    let base = match override_ {
        Some(p) => p.to_path_buf(),
        None => match std::env::var_os("CRABCC_HOME") {
            Some(p) => PathBuf::from(p),
            None => {
                let home = std::env::var_os("HOME")
                    .ok_or_else(|| anyhow!("$HOME is unset; set CRABCC_HOME explicitly"))?;
                PathBuf::from(home).join(".crabcc")
            }
        },
    };
    let dir = base.join("repos");
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(dir)
}

fn ensure_cloned(clone_url: &str, dest: &Path) -> Result<()> {
    if dest.join(".git").exists() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    eprintln!(
        "warning: cloning {} → {} (first use)",
        clone_url,
        dest.display()
    );
    let status = std::process::Command::new("git")
        .args(["clone", "--depth", "1", "--filter=blob:none", clone_url])
        .arg(dest)
        .status()
        .with_context(|| format!("failed to spawn `git clone` for {clone_url}"))?;
    if !status.success() {
        bail!(
            "git clone failed (exit {:?}) for {clone_url} — check the URL, network, and credentials",
            status.code()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};
    use tempfile::tempdir;

    // CRABCC_LAYOUT is process-global; `layout_centralised_env_ignores_dotcrabcc`
    // mutates it via set_var, which races sibling layout assertions under the
    // default parallel test runner. Every test whose expected Layout depends on
    // that env var holds this lock across its resolve. Poison is recovered (a
    // panicking test must not cascade-fail the rest).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_guard() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn url_detection() {
        assert!(is_git_url("https://github.com/foo/bar"));
        assert!(is_git_url("git@github.com:foo/bar.git"));
        assert!(is_git_url("ssh://git@example.com/foo.git"));
        assert!(is_git_url("git://example.com/foo.git"));
        assert!(is_git_url("gh:foo/bar"));
        assert!(!is_git_url("/abs/path"));
        assert!(!is_git_url("./relative"));
        assert!(!is_git_url("relative/path"));
    }

    #[test]
    fn gh_shorthand_expands() {
        let (clone, canon) = canonicalise_url("gh:foo/bar");
        assert_eq!(clone, "https://github.com/foo/bar");
        assert_eq!(canon, "https://github.com/foo/bar");
    }

    #[test]
    fn dotgit_and_trailing_slash_normalised_for_hash() {
        let (_, a) = canonicalise_url("https://github.com/foo/bar");
        let (_, b) = canonicalise_url("https://github.com/foo/bar.git");
        let (_, c) = canonicalise_url("https://github.com/foo/bar/");
        let (_, d) = canonicalise_url("https://github.com/foo/bar.git/");
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(a, d);
        // …so the cache keys collapse too.
        assert_eq!(cache_key(&a), cache_key(&b));
        assert_eq!(cache_key(&a), cache_key(&c));
    }

    #[test]
    fn cache_key_is_16_lowercase_hex() {
        let k = cache_key("anything");
        assert_eq!(k.len(), 16);
        assert!(k
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    fn canon(p: &Path) -> PathBuf {
        // macOS: /var → /private/var symlink; the resolver canonicalizes
        // input paths, so test assertions must compare against the
        // canonical form too.
        p.canonicalize().unwrap()
    }

    #[test]
    fn local_path_with_existing_dotcrabcc_picks_in_repo() {
        let _g = env_guard();
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".crabcc")).unwrap();
        let home = tempdir().unwrap();
        let r = resolve_with_home(&tmp.path().to_string_lossy(), Some(home.path())).unwrap();
        assert_eq!(r.layout, Layout::InRepo);
        assert!(r.db().starts_with(canon(tmp.path())));
        // crabcc_home must NOT be touched in InRepo mode.
        let pollution = home
            .path()
            .join("repos")
            .read_dir()
            .map(|it| it.count())
            .unwrap_or(0);
        assert_eq!(pollution, 0, "crabcc_home was used despite InRepo");
    }

    #[test]
    fn local_path_without_dotcrabcc_picks_centralised() {
        let _g = env_guard();
        let tmp = tempdir().unwrap();
        let home = tempdir().unwrap();
        let r = resolve_with_home(&tmp.path().to_string_lossy(), Some(home.path())).unwrap();
        assert_eq!(r.layout, Layout::Centralised);
        assert!(r.db().starts_with(home.path()));
        assert!(r.key.is_some());
        let key = r.key.as_deref().unwrap();
        assert_eq!(r.data_dir, home.path().join("repos").join(key));
    }

    #[test]
    fn cache_key_stable_across_calls_for_same_path() {
        let tmp = tempdir().unwrap();
        let home = tempdir().unwrap();
        let a = resolve_with_home(&tmp.path().to_string_lossy(), Some(home.path())).unwrap();
        let b = resolve_with_home(&tmp.path().to_string_lossy(), Some(home.path())).unwrap();
        assert_eq!(a.key, b.key);
        assert_eq!(a.data_dir, b.data_dir);
    }

    #[test]
    fn cache_key_differs_for_different_paths() {
        let a_dir = tempdir().unwrap();
        let b_dir = tempdir().unwrap();
        let home = tempdir().unwrap();
        let a = resolve_with_home(&a_dir.path().to_string_lossy(), Some(home.path())).unwrap();
        let b = resolve_with_home(&b_dir.path().to_string_lossy(), Some(home.path())).unwrap();
        assert_ne!(a.key, b.key);
        assert_ne!(a.data_dir, b.data_dir);
    }

    #[test]
    fn nonexistent_local_path_errors_with_actionable_message() {
        let home = tempdir().unwrap();
        let err = resolve_with_home("/this/does/not/exist/9f0c", Some(home.path()))
            .expect_err("should error on missing path");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("--root") && msg.contains("does not exist"),
            "expected actionable error; got: {msg}"
        );
    }

    #[test]
    fn artifact_paths_derive_from_data_dir() {
        let tmp = tempdir().unwrap();
        let home = tempdir().unwrap();
        let r = resolve_with_home(&tmp.path().to_string_lossy(), Some(home.path())).unwrap();
        assert_eq!(r.db(), r.data_dir.join("index.db"));
        assert_eq!(r.fts_dir(), r.data_dir.join("tantivy"));
        assert_eq!(r.graph_json(), r.data_dir.join("graph.json"));
    }

    #[test]
    fn display_origin_local_centralised() {
        let _g = env_guard();
        let tmp = tempdir().unwrap();
        let home = tempdir().unwrap();
        let r = resolve_with_home(&tmp.path().to_string_lossy(), Some(home.path())).unwrap();
        let s = r.display_origin();
        assert!(s.starts_with("local: "), "got `{s}`");
        assert!(s.contains("[key="), "got `{s}`");
    }

    #[test]
    fn display_origin_local_in_repo() {
        let _g = env_guard();
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".crabcc")).unwrap();
        let home = tempdir().unwrap();
        let r = resolve_with_home(&tmp.path().to_string_lossy(), Some(home.path())).unwrap();
        let s = r.display_origin();
        assert!(s.starts_with("local: "), "got `{s}`");
        assert!(s.contains("[in-repo]"), "got `{s}`");
    }

    #[test]
    fn layout_centralised_env_ignores_dotcrabcc() {
        let _g = env_guard();
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".crabcc")).unwrap();
        let home = tempdir().unwrap();
        std::env::set_var("CRABCC_LAYOUT", "centralised");
        let r = resolve_with_home(&tmp.path().to_string_lossy(), Some(home.path())).unwrap();
        std::env::remove_var("CRABCC_LAYOUT");
        assert_eq!(r.layout, Layout::Centralised);
        assert!(r.db().starts_with(home.path()));
    }

    #[test]
    fn repo_storage_key_uses_origin_hash_for_git_repos() {
        let tmp = tempdir().unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/example/crabcc-test.git",
            ])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        let url = "https://github.com/example/crabcc-test.git";
        let hash6: String = sha256_hex(url.as_bytes()).chars().take(6).collect();
        let a = repo_storage_key(tmp.path());
        let b = repo_storage_key(tmp.path());
        assert_eq!(a, b);
        assert!(
            a.ends_with(&hash6),
            "expected slug-hash6 from origin; got {a}, want *-{hash6}"
        );
    }
}
