//! Auto-backup of per-repo `.crabcc/` state.
//!
//! Layout: `<crabcc_home>/backups/<repo-slug>/<unix_ts>/...`. Each
//! snapshot is a directory containing a copy of every artifact under
//! `<repo>/.crabcc/` that's expensive to regenerate:
//!
//! - `index.db` (+ `-shm` / `-wal` if present) — FTS5 + symbols
//! - `tantivy/` — fuzzy + prefix sidecar (recursive)
//! - `graph.json` — call-graph
//! - `memory.db` (+ sidecars) — drawers + vec
//! - `fsst.symbols` — codec
//!
//! Retention: only the last `BACKUPS_KEEP` (default 2) snapshots per
//! repo. Older snapshots are pruned after every successful write.
//!
//! Auto-trigger: callers (`crabcc index`, `crabcc refresh`) opt-in via
//! `auto_snapshot_after_index` once the operation succeeds; failure of
//! the snapshot itself is best-effort (logged at warn) and never
//! propagates back to the caller.

#![allow(dead_code)]

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Number of snapshots to retain per repo. Older ones are pruned
/// after every successful snapshot. Adjustable via env, but the
/// default (2) is the canonical contract for the issue ask.
pub const BACKUPS_KEEP_DEFAULT: usize = 2;
pub const BACKUPS_KEEP_ENV: &str = "CRABCC_BACKUPS_KEEP";

/// Top-level base under which all per-repo backups land. Honours
/// `CRABCC_HOME`, falling back to `~/.crabcc`. The `backups/`
/// subdirectory namespace prevents collisions with the singleton
/// `_internal.db`, `models/`, and `agents/` siblings.
pub fn backups_root(home: &Path) -> PathBuf {
    if let Ok(crabcc_home) = std::env::var("CRABCC_HOME") {
        return PathBuf::from(crabcc_home).join("backups");
    }
    home.join(".crabcc").join("backups")
}

/// Filesystem-safe slug for the repo. Uses the repo's directory name;
/// non-alphanum characters collapse to underscore so the slug is
/// portable across the file systems we care about (APFS, ext4,
/// xfs, NTFS via WSL). Falls back to "unknown-repo" if the path
/// has no terminal component (e.g. `/`).
pub fn repo_slug(repo: &Path) -> String {
    let raw = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown-repo".into());
    let slug: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if slug.is_empty() {
        "unknown-repo".into()
    } else {
        slug
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Files (relative to `<repo>/.crabcc/`) we copy verbatim into the
/// backup directory. Missing files are skipped silently — a fresh
/// repo only has `index.db`; older repos may lack `fsst.symbols`.
const BACKUP_FILES: &[&str] = &[
    "index.db",
    "index.db-shm",
    "index.db-wal",
    "graph.json",
    "memory.db",
    "memory.db-shm",
    "memory.db-wal",
    "fsst.symbols",
];

/// Directories (relative to `<repo>/.crabcc/`) copied recursively.
const BACKUP_DIRS: &[&str] = &["tantivy"];

#[derive(Debug, Clone)]
pub struct SnapshotReport {
    pub destination: PathBuf,
    pub bytes_copied: u64,
    pub files_copied: usize,
    pub dirs_copied: usize,
    pub pruned: usize,
}

/// Take a backup of the current `<repo>/.crabcc/` state. Returns a
/// SnapshotReport even when the source dir is empty (every file is
/// missing) — the destination directory is still created so callers
/// have a stable path to log.
pub fn snapshot(repo_root: &Path, home: &Path) -> Result<SnapshotReport> {
    let src = repo_root.join(".crabcc");
    let slug = repo_slug(repo_root);
    let dst_root = backups_root(home).join(&slug).join(unix_now().to_string());
    std::fs::create_dir_all(&dst_root)
        .with_context(|| format!("create backup dir {}", dst_root.display()))?;

    let mut bytes_copied = 0u64;
    let mut files_copied = 0usize;
    for name in BACKUP_FILES {
        let s = src.join(name);
        if s.is_file() {
            let d = dst_root.join(name);
            let n = std::fs::copy(&s, &d)
                .with_context(|| format!("copy {} → {}", s.display(), d.display()))?;
            bytes_copied += n;
            files_copied += 1;
        }
    }

    let mut dirs_copied = 0usize;
    for name in BACKUP_DIRS {
        let s = src.join(name);
        if s.is_dir() {
            let d = dst_root.join(name);
            let (n_files, n_bytes) = copy_dir_recursive(&s, &d)
                .with_context(|| format!("copy dir {} → {}", s.display(), d.display()))?;
            bytes_copied += n_bytes;
            files_copied += n_files;
            dirs_copied += 1;
        }
    }

    let keep = std::env::var(BACKUPS_KEEP_ENV)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(BACKUPS_KEEP_DEFAULT);
    let pruned = prune_to_n(home, repo_root, keep)?;

    Ok(SnapshotReport {
        destination: dst_root,
        bytes_copied,
        files_copied,
        dirs_copied,
        pruned,
    })
}

#[derive(Debug, Clone)]
pub struct BackupEntry {
    pub timestamp: u64,
    pub path: PathBuf,
    pub bytes: u64,
}

/// List backups for `repo_root`, newest first.
pub fn list(home: &Path, repo_root: &Path) -> Result<Vec<BackupEntry>> {
    let dir = backups_root(home).join(repo_slug(repo_root));
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut out: Vec<BackupEntry> = Vec::new();
    for e in std::fs::read_dir(&dir)?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(ts) = name.parse::<u64>().ok() else {
            continue;
        };
        let p = e.path();
        let bytes = dir_size(&p).unwrap_or(0);
        out.push(BackupEntry {
            timestamp: ts,
            path: p,
            bytes,
        });
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.timestamp));
    Ok(out)
}

/// Delete all but the `keep` most recent backups for this repo.
/// Returns count of backups removed. Best-effort per-entry: a removal
/// failure on one timestamped dir doesn't abort the rest.
pub fn prune_to_n(home: &Path, repo_root: &Path, keep: usize) -> Result<usize> {
    let entries = list(home, repo_root)?;
    let mut removed = 0;
    for old in entries.into_iter().skip(keep) {
        if std::fs::remove_dir_all(&old.path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Restore the named timestamp's backup over `<repo>/.crabcc/`.
/// Existing files at the destination are overwritten; files that
/// existed in `.crabcc/` but NOT in the backup are left untouched
/// (so a partial backup never wipes more than it can replace).
pub fn restore(repo_root: &Path, home: &Path, timestamp: u64) -> Result<usize> {
    let src = backups_root(home)
        .join(repo_slug(repo_root))
        .join(timestamp.to_string());
    if !src.exists() {
        anyhow::bail!("no backup at {}", src.display());
    }
    let dst = repo_root.join(".crabcc");
    std::fs::create_dir_all(&dst)?;
    let mut count = 0;
    for entry in std::fs::read_dir(&src)?.flatten() {
        let name = entry.file_name();
        let s = entry.path();
        let d = dst.join(&name);
        if s.is_dir() {
            // Wipe-and-replace for dirs (tantivy) — partial dir
            // restore is meaningless for the tantivy index.
            let _ = std::fs::remove_dir_all(&d);
            copy_dir_recursive(&s, &d)?;
        } else {
            std::fs::copy(&s, &d)?;
        }
        count += 1;
    }
    Ok(count)
}

/// Best-effort snapshot called from `crabcc index` / `crabcc refresh`
/// after the operation completes successfully. Logs the SnapshotReport
/// at info; logs failures at warn but never propagates an error to the
/// caller — a broken backup must not break the indexing flow.
pub fn auto_snapshot_after_index(repo_root: &Path) {
    snapshot_with_trigger(repo_root, "auto-index");
}

/// Long-running loop driven by the `com.crabcc.backup-loop` LaunchAgent.
/// Snapshots every `interval` seconds against every repo listed in
/// `~/.crabcc/agent/repos.list`. Each snapshot writes a row to
/// `backup_runs` (trigger='auto-loop') so the dashboard's history
/// surface stays accurate.
pub fn run_loop(interval_secs: u64) -> Result<()> {
    let Some(home_os) = std::env::var_os("HOME") else {
        anyhow::bail!("HOME not set");
    };
    let home = PathBuf::from(home_os);
    let repos_list = home.join(".crabcc").join("agent").join("repos.list");
    eprintln!(
        "crabcc backup loop: pid={} interval={}s repos.list={}",
        std::process::id(),
        interval_secs,
        repos_list.display()
    );
    let interval = if interval_secs == 0 {
        900
    } else {
        interval_secs
    };
    loop {
        if let Ok(body) = std::fs::read_to_string(&repos_list) {
            for line in body.lines() {
                let path = line.trim();
                if path.is_empty() || path.starts_with('#') {
                    continue;
                }
                let p = Path::new(path);
                if p.join(".crabcc").is_dir() {
                    snapshot_with_trigger(p, "auto-loop");
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(interval));
    }
}

fn snapshot_with_trigger(repo_root: &Path, trigger: &str) {
    let Some(home_os) = std::env::var_os("HOME") else {
        return;
    };
    let home = PathBuf::from(home_os);
    match snapshot(repo_root, &home) {
        Ok(report) => {
            tracing::info!(
                target: "crabcc_cli::backup",
                destination = %report.destination.display(),
                files_copied = report.files_copied,
                dirs_copied = report.dirs_copied,
                bytes_copied = report.bytes_copied,
                pruned = report.pruned,
                trigger = trigger,
                "snapshot ok"
            );
            // Log into the singleton _internal.db so the dashboard's
            // history pane has a durable record. Best-effort.
            let db_path = crate::agent_runs_db::default_db_path(&home);
            if let Ok(conn) = crate::agent_runs_db::open(&db_path) {
                let _ = crate::agent_runs_db::record_backup(
                    &conn,
                    &repo_root.display().to_string(),
                    &report.destination.display().to_string(),
                    report.files_copied as i64,
                    report.dirs_copied as i64,
                    report.bytes_copied as i64,
                    report.pruned as i64,
                    trigger,
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                target: "crabcc_cli::backup",
                error = format!("{e:#}"),
                trigger = trigger,
                "snapshot failed (non-fatal)"
            );
        }
    }
}

// ---- helpers --------------------------------------------------------------

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(usize, u64)> {
    std::fs::create_dir_all(dst)?;
    let mut files = 0usize;
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(src)?.flatten() {
        let s = entry.path();
        let d = dst.join(entry.file_name());
        if s.is_dir() {
            let (sub_f, sub_b) = copy_dir_recursive(&s, &d)?;
            files += sub_f;
            bytes += sub_b;
        } else if s.is_file() {
            let n = std::fs::copy(&s, &d)?;
            files += 1;
            bytes += n;
        }
        // symlinks intentionally skipped — repos rarely have them
        // under .crabcc/ and following them blindly is a footgun.
    }
    Ok((files, bytes))
}

fn dir_size(p: &Path) -> Result<u64> {
    let mut total = 0u64;
    if p.is_file() {
        return Ok(std::fs::metadata(p)?.len());
    }
    for entry in std::fs::read_dir(p)?.flatten() {
        let sub = entry.path();
        if sub.is_dir() {
            total += dir_size(&sub)?;
        } else {
            total += std::fs::metadata(&sub).map(|m| m.len()).unwrap_or(0);
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn seed_repo(repo: &Path) {
        let crabcc = repo.join(".crabcc");
        std::fs::create_dir_all(crabcc.join("tantivy")).unwrap();
        std::fs::write(crabcc.join("index.db"), b"fake-sqlite").unwrap();
        std::fs::write(crabcc.join("graph.json"), b"{}").unwrap();
        std::fs::write(crabcc.join("memory.db"), b"fake-memory").unwrap();
        std::fs::write(crabcc.join("fsst.symbols"), b"\x00\x01\x02").unwrap();
        std::fs::write(crabcc.join("tantivy").join("meta.json"), b"meta").unwrap();
    }

    #[test]
    fn snapshot_copies_all_known_artifacts_and_keeps_only_last_n() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        seed_repo(repo.path());

        // Three snapshots, distinct timestamps. Verify the file/dir
        // counts at the moment each one is taken (pre-prune of older
        // peers). After all 3 land, only the last 2 should remain on
        // disk per the default retention.
        let r1 = snapshot(repo.path(), home.path()).unwrap();
        assert_eq!(r1.files_copied, 5);
        assert_eq!(r1.dirs_copied, 1);
        std::thread::sleep(std::time::Duration::from_secs(1));
        let r2 = snapshot(repo.path(), home.path()).unwrap();
        assert_eq!(r2.files_copied, 5);
        std::thread::sleep(std::time::Duration::from_secs(1));
        let r3 = snapshot(repo.path(), home.path()).unwrap();
        assert_eq!(r3.files_copied, 5);

        // After 3rd snapshot, the 1st should be pruned (default keep=2).
        assert!(!r1.destination.exists(), "r1 should be pruned after r3");
        assert!(r2.destination.exists());
        assert!(r3.destination.exists());

        let entries = list(home.path(), repo.path()).unwrap();
        assert_eq!(entries.len(), 2);
        // Sorted newest-first.
        assert!(entries[0].timestamp >= entries[1].timestamp);
    }

    #[test]
    fn snapshot_skips_missing_optional_files() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        // Only the index.db exists — no graph, no memory, no tantivy.
        std::fs::create_dir_all(repo.path().join(".crabcc")).unwrap();
        std::fs::write(repo.path().join(".crabcc").join("index.db"), b"x").unwrap();

        let r = snapshot(repo.path(), home.path()).unwrap();
        assert_eq!(r.files_copied, 1);
        assert_eq!(r.dirs_copied, 0);
        assert!(r.destination.join("index.db").exists());
    }

    #[test]
    fn restore_overwrites_destination_with_backup_contents() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        seed_repo(repo.path());

        let r = snapshot(repo.path(), home.path()).unwrap();

        // Mutate the live copy after the snapshot.
        std::fs::write(repo.path().join(".crabcc").join("graph.json"), b"DIRTY").unwrap();
        assert_eq!(
            std::fs::read(repo.path().join(".crabcc").join("graph.json")).unwrap(),
            b"DIRTY"
        );

        let restored = restore(
            repo.path(),
            home.path(),
            r.destination
                .file_name()
                .unwrap()
                .to_string_lossy()
                .parse()
                .unwrap(),
        )
        .unwrap();
        assert!(restored >= 5);
        assert_eq!(
            std::fs::read(repo.path().join(".crabcc").join("graph.json")).unwrap(),
            b"{}"
        );
    }

    #[test]
    fn repo_slug_replaces_separators_with_underscore() {
        assert_eq!(repo_slug(Path::new("/a/b/my-repo")), "my-repo");
        assert_eq!(repo_slug(Path::new("/a/b/my repo!")), "my_repo_");
        assert_eq!(repo_slug(Path::new("")), "unknown-repo");
    }

    #[test]
    fn list_returns_empty_when_no_backups_yet() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        assert!(list(home.path(), repo.path()).unwrap().is_empty());
    }
}
