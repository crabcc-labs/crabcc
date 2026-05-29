//! Cross-repo (`--workspace`) query support.
//!
//! v4.5 ships the first cut of polyrepo symbolic lookup. The discovery
//! mechanism is a **filesystem walk over `$CRABCC_HOME/repos/*/`**, chosen
//! over a manifest file or virtual-table-with-ATTACH because:
//!
//! * Zero new code path — leverages the existing centralised-layout work
//!   shipped in commit 5a26ee4 (`feat(cli): centralised Mac index shared
//!   across git worktrees (#581)`).
//! * Always consistent — no cache to invalidate, no manifest write
//!   coordination between concurrent `crabcc index` calls.
//! * Fast enough — at ~60 repos `read_dir($CRABCC_HOME/repos/)` is
//!   microseconds; the cost dominates further down at per-repo
//!   `Store::open()`.
//!
//! The virtual-table-with-lazy-ATTACH refactor is on the v5.x roadmap
//! (SQLite ATTACH default limit = 10 dbs caps the pure-SQL path at small
//! workspaces; current user has ~60 repos, so materialized union via
//! iteration ships v4.5).
//!
//! Output shape: one envelope per `--workspace` query —
//!
//! ```json
//! {
//!   "workspace": true,
//!   "queried_repos": N,
//!   "total_hits": M,
//!   "by_repo": [
//!     {"repo": "<slug-hash6>", "count": K, "hits": [...]},
//!     ...
//!   ]
//! }
//! ```
//!
//! Repos with zero hits are still listed (count=0, hits=[]) so the agent
//! sees the full search surface and doesn't infer "skipped" as "missing".

use anyhow::{Context, Result};
use std::path::PathBuf;

/// A single repo's index location, discovered via filesystem walk.
#[derive(Debug, Clone)]
pub struct WorkspaceRepo {
    /// Directory name under `$CRABCC_HOME/repos/`. Stable across invocations
    /// for the same `remote.origin.url` (see `root_resolver::repo_storage_key`).
    pub key: String,
    /// `$CRABCC_HOME/repos/<key>/` — the per-repo data dir holding
    /// `index.db`, `tantivy/`, and `graph.json`.
    pub data_dir: PathBuf,
}

impl WorkspaceRepo {
    pub fn db(&self) -> PathBuf {
        self.data_dir.join("index.db")
    }
    pub fn fts_dir(&self) -> PathBuf {
        self.data_dir.join("tantivy")
    }
}

/// Discover every repo currently under `$CRABCC_HOME/repos/*/`.
///
/// Skips entries that aren't directories or that lack an `index.db` — those
/// are partial/in-progress indexes and shouldn't surface in cross-repo
/// queries until they complete.
///
/// Honors `CRABCC_HOME` env var; falls back to `~/.crabcc/`. Returns an empty
/// vec (not an error) when the repos dir doesn't exist yet — users who haven't
/// indexed anything centrally get a clean "0 results" envelope rather than a
/// confusing error.
pub fn discover() -> Result<Vec<WorkspaceRepo>> {
    discover_with_home(None)
}

/// Same as `discover` but with an explicit `$CRABCC_HOME` override. Tests use
/// this to avoid the env-var mutation race that `cargo test` parallelism
/// causes (mirrors `root_resolver::resolve_with_home`).
pub(crate) fn discover_with_home(home_override: Option<&std::path::Path>) -> Result<Vec<WorkspaceRepo>> {
    let home = crabcc_home(home_override)?;
    let repos_dir = home.join("repos");
    if !repos_dir.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&repos_dir)
        .with_context(|| format!("read {}", repos_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        // file_type() does not follow symlinks; a symlink planted in repos/
        // must not let us open an index.db outside the repos tree.
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if !path.join("index.db").exists() {
            continue;
        }
        let key = entry.file_name().to_string_lossy().into_owned();
        out.push(WorkspaceRepo {
            key,
            data_dir: path,
        });
    }
    // Deterministic order so tests and agent diffs are stable.
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

fn crabcc_home(override_: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(p) = override_ {
        return Ok(p.to_path_buf());
    }
    if let Some(p) = std::env::var_os("CRABCC_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow::anyhow!("$HOME is unset; set CRABCC_HOME explicitly"))?;
    Ok(PathBuf::from(home).join(".crabcc"))
}

/// One repo's results, ready for JSON serialisation.
#[derive(serde::Serialize)]
pub struct RepoHits<T> {
    pub repo: String,
    pub count: usize,
    pub hits: T,
}

/// Top-level envelope for any `--workspace` query.
#[derive(serde::Serialize)]
pub struct WorkspaceEnvelope<T> {
    pub workspace: bool,
    pub queried_repos: usize,
    pub total_hits: usize,
    pub by_repo: Vec<RepoHits<T>>,
}

impl<T> WorkspaceEnvelope<T> {
    pub fn new(by_repo: Vec<RepoHits<T>>) -> Self {
        let queried_repos = by_repo.len();
        let total_hits = by_repo.iter().map(|r| r.count).sum();
        Self {
            workspace: true,
            queried_repos,
            total_hits,
            by_repo,
        }
    }
}

/// Run a query against each repo's database+FTS, collecting per-repo hits.
///
/// The closure receives the open paths for one repo at a time so callers
/// can decide which subsystems to materialise (`Store::open` is heavier than
/// `Fts::open`; not every query needs both). Errors from one repo are
/// **logged to stderr and skipped** — a single corrupt index shouldn't
/// fail the whole workspace query.
pub fn map_each<T, F>(repos: &[WorkspaceRepo], mut query: F) -> Vec<RepoHits<T>>
where
    F: FnMut(&WorkspaceRepo) -> Result<(usize, T)>,
    T: Default,
{
    let mut out = Vec::with_capacity(repos.len());
    for repo in repos {
        match query(repo) {
            Ok((count, hits)) => out.push(RepoHits {
                repo: repo.key.clone(),
                count,
                hits,
            }),
            Err(e) => {
                eprintln!(
                    "workspace: skipping {} ({}): {:#}",
                    repo.key,
                    repo.data_dir.display(),
                    e
                );
                out.push(RepoHits {
                    repo: repo.key.clone(),
                    count: 0,
                    hits: T::default(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn make_repo(home: &Path, key: &str, with_db: bool) -> PathBuf {
        let p = home.join("repos").join(key);
        std::fs::create_dir_all(&p).unwrap();
        if with_db {
            std::fs::write(p.join("index.db"), b"").unwrap();
        }
        p
    }

    #[test]
    fn discover_returns_empty_when_repos_dir_missing() {
        let home = tempdir().unwrap();
        let repos = discover_with_home(Some(home.path())).unwrap();
        assert!(repos.is_empty());
    }

    #[test]
    fn discover_skips_dirs_without_index_db() {
        let home = tempdir().unwrap();
        make_repo(home.path(), "with-db-abc123", true);
        make_repo(home.path(), "without-db-def456", false);
        let repos = discover_with_home(Some(home.path())).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].key, "with-db-abc123");
    }

    #[test]
    fn discover_returns_sorted_keys() {
        let home = tempdir().unwrap();
        make_repo(home.path(), "zebra-aaa111", true);
        make_repo(home.path(), "apple-bbb222", true);
        make_repo(home.path(), "mango-ccc333", true);
        let repos = discover_with_home(Some(home.path())).unwrap();
        let keys: Vec<&str> = repos.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, vec!["apple-bbb222", "mango-ccc333", "zebra-aaa111"]);
    }

    #[test]
    fn discover_skips_files_in_repos_dir() {
        let home = tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("repos")).unwrap();
        std::fs::write(home.path().join("repos").join("stray.txt"), b"hello").unwrap();
        make_repo(home.path(), "real-aaa111", true);
        let repos = discover_with_home(Some(home.path())).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].key, "real-aaa111");
    }

    #[test]
    fn envelope_aggregates_counts() {
        let by_repo = vec![
            RepoHits {
                repo: "a-aaa111".into(),
                count: 3,
                hits: vec!["x", "y", "z"],
            },
            RepoHits {
                repo: "b-bbb222".into(),
                count: 0,
                hits: vec![],
            },
            RepoHits {
                repo: "c-ccc333".into(),
                count: 2,
                hits: vec!["p", "q"],
            },
        ];
        let env = WorkspaceEnvelope::new(by_repo);
        assert!(env.workspace);
        assert_eq!(env.queried_repos, 3);
        assert_eq!(env.total_hits, 5);
    }

    #[test]
    fn map_each_swallows_per_repo_errors() {
        let home = tempdir().unwrap();
        make_repo(home.path(), "ok-aaa111", true);
        make_repo(home.path(), "broken-bbb222", true);

        let repos = vec![
            WorkspaceRepo {
                key: "ok-aaa111".into(),
                data_dir: home.path().join("repos").join("ok-aaa111"),
            },
            WorkspaceRepo {
                key: "broken-bbb222".into(),
                data_dir: home.path().join("repos").join("broken-bbb222"),
            },
        ];

        let results: Vec<RepoHits<Vec<u32>>> = map_each(&repos, |r| {
            if r.key.starts_with("broken") {
                anyhow::bail!("simulated failure");
            }
            Ok((1, vec![42]))
        });

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].count, 1);
        assert_eq!(results[0].hits, vec![42]);
        assert_eq!(results[1].count, 0);
        assert!(results[1].hits.is_empty());
    }
}
