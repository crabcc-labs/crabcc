//! Upgrade check + migration helper.
//!
//! crabcc's repo is private — querying releases via the public GitHub REST
//! API would 404 for users who haven't set `GITHUB_TOKEN`. We shell out to
//! `gh release list --repo OWNER/REPO ...` which inherits the user's existing
//! `gh auth login` credentials and works against private repos out of the box.
//!
//! Surface:
//! - `installed_version()` — compile-time version baked into the binary.
//! - `latest_release(repo)` — one-shot fetch of the most recent release.
//! - `compare_versions(installed, latest)` — pure semver delta classification.
//! - `build_report(repo, root)` — convenience wrapper that builds a structured
//!   summary suitable for both the CLI human-readable path and the MCP /
//!   slash-command JSON path.
//! - `cleanup_index(root)` — `rm -rf .crabcc/index.db .crabcc/tantivy
//!   .crabcc/graph.json` for the case where a major bump's schema changed
//!   (additive in v1, but the function is wired now so the surface is stable).

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

/// Default repo to check. Overridable via `CRABCC_UPGRADE_REPO=owner/name`
/// to support forks / mirrors without recompiling.
pub const DEFAULT_REPO: &str = "peterlodri-sec/crabcc";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseInfo {
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BumpKind {
    Patch,
    Minor,
    Major,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VersionDelta {
    UpToDate,
    Newer {
        current: String,
        latest: String,
        kind: BumpKind,
    },
    /// Local build is ahead of the remote release tag — likely a dev build.
    Ahead {
        current: String,
        latest: String,
    },
    /// `gh` not installed, not authenticated, or returned no releases.
    Unknown {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeReport {
    pub installed: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<ReleaseInfo>,
    pub delta: VersionDelta,
    pub recommendations: Vec<String>,
}

/// Compile-time version pinned at build time from `crabcc-core`'s Cargo.toml.
pub fn installed_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Resolve which repo to query. Honors `CRABCC_UPGRADE_REPO` for forks.
pub fn target_repo() -> String {
    std::env::var("CRABCC_UPGRADE_REPO").unwrap_or_else(|_| DEFAULT_REPO.into())
}

/// Fetch the latest release tag + body via `gh release list ... --limit 1`.
/// Returns `Err` (not `Ok(None)`) for the cases the caller probably wants to
/// surface as a soft "unknown" rather than a hard failure — but the choice is
/// the caller's; we don't paper over an actual missing-binary or auth error.
pub fn latest_release(repo: &str) -> Result<ReleaseInfo> {
    // Pull a small window of recent releases so we can pick the first non-draft,
    // non-prerelease entry. `gh release list` orders by createdAt desc.
    let out = Command::new("gh")
        .args([
            "release",
            "list",
            "--repo",
            repo,
            "--limit",
            "5",
            "--json",
            "tagName,publishedAt,name,isDraft,isPrerelease",
        ])
        .output()
        .map_err(|e| {
            anyhow!(
                "could not run `gh` ({e}) — install it (https://cli.github.com) \
                 and `gh auth login`. crabcc's repo is private; the public \
                 GitHub API alone won't work."
            )
        })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("`gh release list` failed: {stderr}"));
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct GhRelease {
        tag_name: String,
        published_at: Option<String>,
        #[serde(default)]
        is_draft: bool,
        #[serde(default)]
        is_prerelease: bool,
    }

    let releases: Vec<GhRelease> =
        serde_json::from_slice(&out.stdout).context("parse `gh release list` json")?;

    let r = releases
        .into_iter()
        .find(|r| !r.is_draft && !r.is_prerelease)
        .ok_or_else(|| anyhow!("no published releases found for {repo}"))?;

    // Synthesize the canonical release URL from the tag — gh's JSON doesn't
    // expose a `url` field but the shape is fixed.
    let url = Some(format!(
        "https://github.com/{repo}/releases/tag/{}",
        r.tag_name
    ));

    Ok(ReleaseInfo {
        tag: r.tag_name,
        published_at: r.published_at,
        url,
        body: None,
    })
}

/// Pure semver comparison. Both inputs may carry an optional `v` prefix.
/// We DON'T do full SemVer 2.0 — pre-release / build metadata is stripped.
/// That's fine for our purposes: the question is "should the user upgrade?",
/// and a release tag like `v1.2.0-rc.1` should still trigger a "newer".
pub fn compare_versions(installed: &str, latest: &str) -> VersionDelta {
    let parse = |s: &str| -> Option<(u32, u32, u32)> {
        let core = s.trim().trim_start_matches('v').split(['-', '+']).next()?;
        let mut parts = core.split('.').map(str::parse::<u32>);
        let ma = parts.next()?.ok()?;
        let mi = parts.next()?.ok()?;
        let pa = parts.next()?.ok()?;
        Some((ma, mi, pa))
    };

    let i = parse(installed);
    let l = parse(latest);
    match (i, l) {
        (Some(i), Some(l)) => {
            if i == l {
                VersionDelta::UpToDate
            } else if i > l {
                VersionDelta::Ahead {
                    current: installed.to_string(),
                    latest: latest.to_string(),
                }
            } else {
                let kind = if i.0 != l.0 {
                    BumpKind::Major
                } else if i.1 != l.1 {
                    BumpKind::Minor
                } else {
                    BumpKind::Patch
                };
                VersionDelta::Newer {
                    current: installed.to_string(),
                    latest: latest.to_string(),
                    kind,
                }
            }
        }
        _ => VersionDelta::Unknown {
            reason: format!("could not parse versions {installed:?} / {latest:?}"),
        },
    }
}

/// Build a structured upgrade report — used by both the CLI human-readable
/// rendering and the MCP / slash command JSON paths.
///
/// `root` is optional; when present, recommendations include cleanup steps
/// scoped to that repo's `.crabcc/` index.
pub fn build_report(repo: &str, root: Option<&Path>) -> UpgradeReport {
    let installed = installed_version().to_string();
    match latest_release(repo) {
        Ok(rel) => {
            let delta = compare_versions(&installed, &rel.tag);
            let recommendations = recommend(&delta, root);
            UpgradeReport {
                installed,
                latest: Some(rel),
                delta,
                recommendations,
            }
        }
        Err(e) => UpgradeReport {
            installed,
            latest: None,
            delta: VersionDelta::Unknown {
                reason: format!("{e}"),
            },
            recommendations: vec![format!(
                "could not query GitHub: {e}. Install gh (https://cli.github.com) and `gh auth login`."
            )],
        },
    }
}

fn recommend(delta: &VersionDelta, root: Option<&Path>) -> Vec<String> {
    match delta {
        VersionDelta::UpToDate => vec!["already on the latest release.".into()],
        VersionDelta::Ahead { current, latest } => vec![format!(
            "local build {current} is ahead of the latest release {latest} \
             — likely a dev build, no upgrade needed."
        )],
        VersionDelta::Unknown { .. } => Vec::new(),
        VersionDelta::Newer { kind, latest, .. } => {
            let mut out = Vec::new();
            out.push(match kind {
                BumpKind::Patch => format!("patch release {latest} available — safe upgrade."),
                BumpKind::Minor => format!(
                    "minor release {latest} available — additive features, \
                     no breaking changes expected."
                ),
                BumpKind::Major => format!(
                    "MAJOR release {latest} available — review the CHANGELOG \
                     before upgrading; expect breaking changes."
                ),
            });
            out.push(
                "upgrade with `cargo install --git \
                 https://github.com/peterlodri-sec/crabcc --tag <tag>` \
                 or download the binary from the GH Releases page."
                    .into(),
            );
            if matches!(kind, BumpKind::Major) {
                if let Some(r) = root {
                    let p = r.join(".crabcc").join("index.db");
                    if p.exists() {
                        out.push(format!(
                            "after upgrading, clear and reindex: \
                             `rm -rf {} && crabcc index` \
                             (major bumps may change the on-disk schema).",
                            p.display()
                        ));
                    }
                }
            }
            out
        }
    }
}

/// Delete the local `.crabcc/` sidecars (index.db, tantivy/, graph.json).
/// Used by the CLI's `--apply` path when the user opts into a clean migration.
pub fn cleanup_index(root: &Path) -> Result<()> {
    let dir = root.join(".crabcc");
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let p = entry.path();
        // Don't delete the user's `.crabcc/usage.log` if any future code stores
        // it under the repo (currently it lives under `~/.crabcc/`). We only
        // touch index/tantivy/graph artifacts.
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        match name {
            "index.db" | "index.db-shm" | "index.db-wal" | "graph.json" => {
                let _ = std::fs::remove_file(&p);
            }
            "tantivy" => {
                let _ = std::fs::remove_dir_all(&p);
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_version_is_non_empty() {
        let v = installed_version();
        assert!(!v.is_empty());
        // Crude semver shape check (e.g. "1.1.0").
        assert!(v.split('.').count() >= 2, "version not semver-shaped: {v}");
    }

    #[test]
    fn target_repo_defaults_when_env_unset() {
        // Best-effort: don't fight other tests that may set the env. Just
        // assert that target_repo() returns *some* slash-separated owner/repo.
        let r = target_repo();
        assert!(r.contains('/'), "{r:?}");
    }

    #[test]
    fn compare_equal_versions_is_up_to_date() {
        let d = compare_versions("1.0.0", "1.0.0");
        assert_eq!(d, VersionDelta::UpToDate);
    }

    #[test]
    fn compare_strips_v_prefix() {
        let d = compare_versions("1.1.0", "v1.1.0");
        assert_eq!(d, VersionDelta::UpToDate);
    }

    #[test]
    fn compare_classifies_patch_minor_major() {
        match compare_versions("1.0.0", "1.0.1") {
            VersionDelta::Newer { kind, .. } => assert_eq!(kind, BumpKind::Patch),
            other => panic!("expected patch, got {other:?}"),
        }
        match compare_versions("1.0.0", "1.1.0") {
            VersionDelta::Newer { kind, .. } => assert_eq!(kind, BumpKind::Minor),
            other => panic!("expected minor, got {other:?}"),
        }
        match compare_versions("1.0.0", "2.0.0") {
            VersionDelta::Newer { kind, .. } => assert_eq!(kind, BumpKind::Major),
            other => panic!("expected major, got {other:?}"),
        }
    }

    #[test]
    fn compare_local_ahead_detected() {
        let d = compare_versions("2.0.0", "1.0.0");
        assert!(matches!(d, VersionDelta::Ahead { .. }));
    }

    #[test]
    fn compare_strips_prerelease_suffix() {
        // `v1.2.0-rc.1` should still be treated as 1.2.0 for upgrade purposes.
        let d = compare_versions("1.0.0", "v1.2.0-rc.1");
        match d {
            VersionDelta::Newer { kind, .. } => assert_eq!(kind, BumpKind::Minor),
            other => panic!("expected minor, got {other:?}"),
        }
    }

    #[test]
    fn compare_garbage_inputs_yield_unknown() {
        assert!(matches!(
            compare_versions("not-a-version", "1.0.0"),
            VersionDelta::Unknown { .. }
        ));
        assert!(matches!(
            compare_versions("1.0.0", ""),
            VersionDelta::Unknown { .. }
        ));
    }

    #[test]
    fn build_report_serializes_to_json() {
        // Doesn't actually call `gh` — feeds an Unknown delta path. We only
        // care that the structured shape round-trips through serde so MCP
        // and slash-command consumers get a stable contract.
        let r = UpgradeReport {
            installed: "1.0.0".into(),
            latest: None,
            delta: VersionDelta::Unknown {
                reason: "test".into(),
            },
            recommendations: vec!["foo".into()],
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"installed\":\"1.0.0\""));
        assert!(json.contains("\"status\":\"unknown\""));
        let back: UpgradeReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.installed, "1.0.0");
    }

    #[test]
    fn report_with_newer_includes_upgrade_command() {
        let recs = recommend(
            &VersionDelta::Newer {
                current: "1.0.0".into(),
                latest: "1.1.0".into(),
                kind: BumpKind::Minor,
            },
            None,
        );
        assert!(recs.iter().any(|r| r.contains("cargo install")));
    }

    #[test]
    fn cleanup_index_is_idempotent_on_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        // No .crabcc/ — should not error.
        cleanup_index(dir.path()).unwrap();
    }

    #[test]
    fn cleanup_index_removes_db_and_tantivy_only() {
        let dir = tempfile::tempdir().unwrap();
        let crabcc = dir.path().join(".crabcc");
        std::fs::create_dir_all(&crabcc).unwrap();
        std::fs::write(crabcc.join("index.db"), b"fake").unwrap();
        std::fs::write(crabcc.join("graph.json"), b"{}").unwrap();
        std::fs::create_dir_all(crabcc.join("tantivy")).unwrap();
        std::fs::write(crabcc.join("tantivy").join("a.idx"), b"x").unwrap();
        // Drop a sentinel that should NOT be deleted.
        std::fs::write(crabcc.join("user_note.md"), b"keep me").unwrap();

        cleanup_index(dir.path()).unwrap();

        assert!(!crabcc.join("index.db").exists());
        assert!(!crabcc.join("graph.json").exists());
        assert!(!crabcc.join("tantivy").exists());
        assert!(
            crabcc.join("user_note.md").exists(),
            "user file must survive"
        );
    }

    #[test]
    fn recommend_up_to_date_says_already_latest() {
        let recs = recommend(&VersionDelta::UpToDate, None);
        assert_eq!(recs.len(), 1);
        assert!(recs[0].contains("latest"), "got: {:?}", recs[0]);
    }

    #[test]
    fn recommend_ahead_mentions_dev_build() {
        let recs = recommend(
            &VersionDelta::Ahead {
                current: "2.0.0".into(),
                latest: "1.9.0".into(),
            },
            None,
        );
        assert_eq!(recs.len(), 1);
        assert!(
            recs[0].contains("dev build") || recs[0].contains("ahead"),
            "got: {:?}",
            recs[0]
        );
    }

    #[test]
    fn recommend_major_without_root_has_no_cleanup_step() {
        let recs = recommend(
            &VersionDelta::Newer {
                current: "1.0.0".into(),
                latest: "2.0.0".into(),
                kind: BumpKind::Major,
            },
            None,
        );
        // Must contain the major upgrade warning but no cleanup path (no root).
        assert!(recs.iter().any(|r| r.contains("MAJOR")));
        assert!(!recs.iter().any(|r| r.contains("crabcc index")));
    }

    #[test]
    fn recommend_major_with_existing_index_includes_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let crabcc = dir.path().join(".crabcc");
        std::fs::create_dir_all(&crabcc).unwrap();
        std::fs::write(crabcc.join("index.db"), b"fake").unwrap();

        let recs = recommend(
            &VersionDelta::Newer {
                current: "1.0.0".into(),
                latest: "2.0.0".into(),
                kind: BumpKind::Major,
            },
            Some(dir.path()),
        );
        assert!(
            recs.iter().any(|r| r.contains("crabcc index")),
            "major bump with existing index.db must suggest re-index: {recs:?}"
        );
    }

    #[test]
    fn compare_strips_build_metadata() {
        // `v1.2.0+build.42` should still be treated as 1.2.0.
        let d = compare_versions("1.0.0", "v1.2.0+build.42");
        match d {
            VersionDelta::Newer { kind, .. } => assert_eq!(kind, BumpKind::Minor),
            other => panic!("expected minor, got {other:?}"),
        }
    }

    #[test]
    fn recommend_unknown_returns_no_recommendations() {
        let recs = recommend(&VersionDelta::Unknown { reason: "test".into() }, None);
        assert!(recs.is_empty(), "unknown delta should produce no recs");
    }
}
