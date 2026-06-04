// GitHub (and, later, Gitea) API client for PR data.
//
// Configuration via env vars:
//   CRABCC_FORGE_TOKEN  — personal access token (optional; raises rate limit)
//   CRABCC_FORGE_REPO   — "owner/repo" (overrides auto-detect from git remote)
//
// Uses ureq (blocking) to fit tiny_http's sync dispatch model.
// Responses are fresh-fetched per request; call-site caching is in lib.rs.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

// ── Public API types ──────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PrAuthor {
    pub login: String,
    pub avatar_url: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PrLabel {
    pub name: String,
    pub color: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    /// "open" | "closed"
    pub state: String,
    pub draft: bool,
    pub merged: bool,
    pub author: PrAuthor,
    pub head_ref: String,
    pub base_ref: String,
    pub created_at: String,
    pub updated_at: String,
    pub labels: Vec<PrLabel>,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub html_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PrFile {
    pub filename: String,
    /// "added" | "modified" | "removed" | "renamed"
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_filename: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct PrDetail {
    pub pr: PrSummary,
    pub files: Vec<PrFile>,
    /// True when the PR has more than 300 changed files and the file list is capped.
    pub files_truncated: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct PrListResponse {
    pub prs: Vec<PrSummary>,
    pub repo: String,
    pub total: usize,
    pub page: u32,
}

#[derive(Serialize, Clone, Debug)]
pub struct ForgeConfig {
    pub repo: String,
    pub configured: bool,
    pub token_present: bool,
    pub forge_kind: &'static str,
    /// Cached from the last successful GitHub API response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_remaining: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_reset: Option<i64>,
}

/// A node in the PR impact (blast-radius) graph.
#[derive(Serialize, Clone, Debug)]
pub struct ImpactNode {
    /// Symbol qualified name or file path for file-only nodes.
    pub id: String,
    pub label: String,
    pub file: String,
    pub kind: String,
    /// true = the symbol lives in a file changed by this PR.
    pub changed: bool,
    /// 0 = changed, 1 = direct dep, 2 = transitive dep.
    pub depth: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ImpactEdge {
    pub src: String,
    pub dst: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct PrImpactGraph {
    pub pr_number: u64,
    pub changed_files: Vec<String>,
    pub nodes: Vec<ImpactNode>,
    pub edges: Vec<ImpactEdge>,
    pub direct_symbols: usize,
    pub impacted_symbols: usize,
    /// true when the file list or node list was capped — the graph is incomplete.
    pub truncated: bool,
}

// ── Structured HTTP error — carries status code back to the route handler ─

#[derive(Debug)]
pub struct ForgeHttpError {
    pub status: u16,
}

impl std::fmt::Display for ForgeHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            401 => write!(f, "HTTP 401: unauthorized — check CRABCC_FORGE_TOKEN"),
            403 => write!(
                f,
                "HTTP 403: forbidden — token lacks permissions or is rate-limited"
            ),
            404 => write!(f, "HTTP 404: not found — check CRABCC_FORGE_REPO"),
            n => write!(f, "HTTP {n}: GitHub API error"),
        }
    }
}

impl std::error::Error for ForgeHttpError {}

// ── Shared GitHub JSON deserialization structs (module-private) ──────────

#[derive(Deserialize)]
struct GhUser {
    login: String,
    avatar_url: String,
}

#[derive(Deserialize)]
struct GhRef {
    #[serde(rename = "ref")]
    ref_: String,
}

#[derive(Deserialize)]
struct GhLabel {
    name: String,
    color: String,
}

#[derive(Deserialize)]
struct GhPr {
    number: u64,
    title: String,
    state: String,
    draft: bool,
    merged_at: Option<String>,
    user: GhUser,
    head: GhRef,
    base: GhRef,
    created_at: String,
    updated_at: String,
    labels: Vec<GhLabel>,
    additions: Option<u64>,
    deletions: Option<u64>,
    changed_files: Option<u64>,
    html_url: String,
    body: Option<String>,
}

fn gh_pr_to_summary(p: GhPr) -> PrSummary {
    PrSummary {
        number: p.number,
        title: p.title,
        state: p.state,
        draft: p.draft,
        merged: p.merged_at.is_some(),
        author: PrAuthor {
            login: p.user.login,
            avatar_url: p.user.avatar_url,
        },
        head_ref: p.head.ref_,
        base_ref: p.base.ref_,
        created_at: p.created_at,
        updated_at: p.updated_at,
        labels: p
            .labels
            .into_iter()
            .map(|l| PrLabel {
                name: l.name,
                color: l.color,
            })
            .collect(),
        additions: p.additions.unwrap_or_default(),
        deletions: p.deletions.unwrap_or_default(),
        changed_files: p.changed_files.unwrap_or_default(),
        html_url: p.html_url,
        body: p.body,
    }
}

// ── Rate-limit cache ──────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct RateLimit {
    remaining: Option<i64>,
    reset: Option<i64>,
}

static RATE_LIMIT_CACHE: Mutex<RateLimit> = Mutex::new(RateLimit {
    remaining: None,
    reset: None,
});

fn absorb_rate_limit_headers(resp: &ureq::Response) {
    let remaining = resp
        .header("X-RateLimit-Remaining")
        .and_then(|v| v.parse().ok());
    let reset = resp
        .header("X-RateLimit-Reset")
        .and_then(|v| v.parse().ok());
    if remaining.is_some() || reset.is_some() {
        if let Ok(mut rl) = RATE_LIMIT_CACHE.lock() {
            if let Some(r) = remaining {
                rl.remaining = Some(r);
            }
            if let Some(r) = reset {
                rl.reset = Some(r);
            }
        }
    }
}

fn cached_rate_limit() -> (Option<i64>, Option<i64>) {
    RATE_LIMIT_CACHE
        .lock()
        .map(|rl| (rl.remaining, rl.reset))
        .unwrap_or((None, None))
}

// ── Config detection ──────────────────────────────────────────────────────

/// Detect "owner/repo" from `git remote get-url origin` in `root`.
pub fn detect_repo(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let result = parse_github_repo(&url);
    if result.is_none() {
        tracing::warn!("crabcc-viz: no GitHub repo matched for remote URL: {url:?}");
    }
    result
}

fn parse_github_repo(url: &str) -> Option<String> {
    // SSH: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let slug = rest.trim_end_matches(".git");
        if slug.contains('/') {
            return Some(slug.to_string());
        }
    }
    // HTTPS: https://github.com/owner/repo[.git]
    for prefix in &["https://github.com/", "http://github.com/"] {
        if let Some(rest) = url.strip_prefix(prefix) {
            let slug = rest.trim_end_matches(".git");
            if slug.contains('/') {
                return Some(slug.to_string());
            }
        }
    }
    None
}

fn token() -> Option<String> {
    std::env::var("CRABCC_FORGE_TOKEN")
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .ok()
        .filter(|s| !s.is_empty())
}

pub fn forge_config(root: &Path) -> ForgeConfig {
    let repo = std::env::var("CRABCC_FORGE_REPO")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| detect_repo(root))
        .unwrap_or_default();
    let token_present = token().is_some();
    let configured = !repo.is_empty();
    let (rate_limit_remaining, rate_limit_reset) = cached_rate_limit();
    ForgeConfig {
        repo,
        configured,
        token_present,
        forge_kind: "github",
        rate_limit_remaining,
        rate_limit_reset,
    }
}

// ── HTTP helper ───────────────────────────────────────────────────────────

fn github_get(url: &str) -> Result<ureq::Response> {
    let mut req = ureq::get(url)
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .set("User-Agent", "crabcc-viz/4");
    if let Some(tok) = token() {
        req = req.set("Authorization", &format!("Bearer {tok}"));
    }
    match req.call() {
        Ok(resp) => {
            absorb_rate_limit_headers(&resp);
            Ok(resp)
        }
        Err(ureq::Error::Status(code, _)) => Err(ForgeHttpError { status: code }.into()),
        Err(e) => Err(e).context("GitHub API request failed"),
    }
}

// ── API methods ────────────────────────────────────────────────────────────

pub fn list_prs(root: &Path, state: &str, page: u32) -> Result<PrListResponse> {
    let cfg = forge_config(root);
    if !cfg.configured {
        bail!(
            "no GitHub repo detected — set CRABCC_FORGE_REPO=owner/repo or push to a GitHub remote"
        );
    }
    let repo = &cfg.repo;
    let state = match state {
        "open" | "closed" | "all" => state,
        _ => "open",
    };
    let url =
        format!("https://api.github.com/repos/{repo}/pulls?state={state}&per_page=30&page={page}");
    let resp = github_get(&url)?;
    let raw: Vec<GhPr> = resp.into_json()?;
    let total = raw.len();
    let prs = raw.into_iter().map(gh_pr_to_summary).collect();
    Ok(PrListResponse {
        prs,
        repo: repo.clone(),
        total,
        page,
    })
}

pub fn get_pr(root: &Path, number: u64) -> Result<PrSummary> {
    let cfg = forge_config(root);
    if !cfg.configured {
        bail!("no GitHub repo detected");
    }
    let repo = &cfg.repo;
    let url = format!("https://api.github.com/repos/{repo}/pulls/{number}");
    let resp = github_get(&url)?;
    let p: GhPr = resp.into_json()?;
    Ok(gh_pr_to_summary(p))
}

/// Fetch changed files for a PR, paginating up to `FILES_PAGE_LIMIT` pages
/// (300 files). Returns `(files, truncated)` — `truncated` is true when the
/// PR has more files than the cap.
pub fn get_pr_files(root: &Path, number: u64) -> Result<(Vec<PrFile>, bool)> {
    let cfg = forge_config(root);
    if !cfg.configured {
        bail!("no GitHub repo detected");
    }
    let repo = &cfg.repo;

    #[derive(Deserialize)]
    struct GhFile {
        filename: String,
        status: String,
        additions: u64,
        deletions: u64,
        patch: Option<String>,
        previous_filename: Option<String>,
    }

    let mut all: Vec<PrFile> = Vec::new();
    let mut truncated = false;

    for page in 1..=FILES_PAGE_LIMIT {
        let url = format!(
            "https://api.github.com/repos/{repo}/pulls/{number}/files?per_page=100&page={page}"
        );
        let resp = github_get(&url)?;
        let batch: Vec<GhFile> = resp.into_json()?;
        let batch_len = batch.len();
        all.extend(batch.into_iter().map(|f| PrFile {
            filename: f.filename,
            status: f.status,
            additions: f.additions,
            deletions: f.deletions,
            patch: f.patch,
            previous_filename: f.previous_filename,
        }));
        if batch_len < 100 {
            break; // reached last page
        }
        if page == FILES_PAGE_LIMIT {
            truncated = true; // hit the 300-file cap
        }
    }

    Ok((all, truncated))
}

/// Build the blast-radius graph for a PR.
///
/// Algorithm:
///  1. Fetch the list of files changed in the PR (up to 300; sets `truncated`).
///  2. For each changed file, look up symbols defined there in the
///     crabcc symbol index (direct SQL query against `index.db`).
///  3. For each symbol, walk one hop of callers + callees in
///     `graph.json` to find immediate dependencies.
///  4. Return the induced sub-graph capped at MAX_IMPACT_NODES.
pub fn pr_impact_graph(root: &Path, number: u64) -> Result<PrImpactGraph> {
    let (files, file_truncated) = get_pr_files(root, number)?;
    let changed_files: std::collections::HashSet<String> =
        files.iter().map(|f| f.filename.clone()).collect();

    let db_path = root.join(".crabcc").join("index.db");
    let graph_path = root.join(".crabcc").join("graph.json");

    // Early-exit if no index — return file-only graph.
    if !db_path.exists() {
        let nodes: Vec<ImpactNode> = changed_files
            .iter()
            .map(|f| ImpactNode {
                id: f.clone(),
                label: short_path(f),
                file: f.clone(),
                kind: "file".into(),
                changed: true,
                depth: 0,
                line: None,
            })
            .collect();
        return Ok(PrImpactGraph {
            pr_number: number,
            changed_files: changed_files.into_iter().collect(),
            direct_symbols: nodes.len(),
            impacted_symbols: 0,
            truncated: file_truncated,
            nodes,
            edges: vec![],
        });
    }

    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    // Collect symbols from changed files.
    // Also build name→id from file-scoped lookups to avoid ambiguity when
    // multiple files define the same symbol name.
    let mut changed_symbols: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut node_map: std::collections::HashMap<String, ImpactNode> =
        std::collections::HashMap::new();
    let mut name_to_id: std::collections::HashMap<String, i64> = std::collections::HashMap::new();

    'outer: for file in &changed_files {
        let mut stmt = conn.prepare_cached(
            "SELECT s.id, s.name, s.kind, s.line_start \
             FROM symbols s JOIN files f ON f.id = s.file_id \
             WHERE f.path = ?1 AND s.kind IN ('function','method','struct','enum','trait','class') \
             ORDER BY s.line_start LIMIT 200",
        )?;
        let rows = stmt.query_map(rusqlite::params![file], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?;
        // Per-file sub-cap: no single file may consume more than half the
        // budget, so symbol-heavy generated files can't starve other changed
        // files of representation.
        let per_file_cap = node_map.len() + (MAX_IMPACT_NODES / 2).max(1);
        for row in rows.flatten() {
            if node_map.len() >= MAX_IMPACT_NODES {
                break 'outer;
            }
            if node_map.len() >= per_file_cap {
                break;
            }
            let (sym_id, name, kind, line) = row;
            // Use sym_id as the node key/id so two files that define the same
            // symbol name get distinct entries instead of the later overwriting
            // the earlier.  The human-readable `label` still shows the short
            // name; `id` is opaque to the frontend (used only for D3 link
            // resolution, which accepts any unique string).
            let node_id = sym_id.to_string();
            let node = ImpactNode {
                id: node_id.clone(),
                label: short_name(&name),
                file: file.clone(),
                kind,
                changed: true,
                depth: 0,
                line: line.map(|l| l as u32),
            };
            changed_symbols.insert(name.clone());
            name_to_id.insert(node_id.clone(), sym_id);
            node_map.insert(node_id, node);
        }
    }

    // Walk one hop of the call graph to find impacted symbols.
    // The v4 CallGraph uses i64 node IDs; we bridge to names via the Store.
    let mut edges: Vec<ImpactEdge> = Vec::new();
    // O(1) dedup set mirrors `edges` to avoid O(n²) linear scan.
    let mut edge_set: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();

    if graph_path.exists() {
        if let (Ok(graph), Ok(store)) = (
            crabcc_core::graph::CallGraph::load(&graph_path),
            crabcc_core::store::Store::open(&db_path),
        ) {
            // name_to_id was already populated with file-scoped IDs above.

            let mut to_add: Vec<(i64, u32)> = Vec::new(); // (id, depth)
            for (sym, &sym_id) in &name_to_id {
                // Direct callers.
                if let Some(callers) = graph.callers.get(&sym_id) {
                    for &caller_id in callers.iter().take(20) {
                        if let Ok(Some(caller_name)) = store.symbol_name_by_id(caller_id) {
                            if !changed_symbols.contains(&caller_name) {
                                to_add.push((caller_id, 1));
                            }
                            let key = (caller_name.clone(), sym.clone());
                            if edge_set.insert(key) {
                                edges.push(ImpactEdge {
                                    src: caller_name,
                                    dst: sym.clone(),
                                });
                            }
                        }
                    }
                }
                // Direct callees.
                if let Some(callees) = graph.callees.get(&sym_id) {
                    for &callee_id in callees.iter().take(10) {
                        if let Ok(Some(callee_name)) = store.symbol_name_by_id(callee_id) {
                            if !changed_symbols.contains(&callee_name) {
                                to_add.push((callee_id, 1));
                            }
                            let key = (sym.clone(), callee_name.clone());
                            if edge_set.insert(key) {
                                edges.push(ImpactEdge {
                                    src: sym.clone(),
                                    dst: callee_name,
                                });
                            }
                        }
                    }
                }
            }

            // Enrich the impacted symbols with metadata.
            for (sym_id, depth) in to_add {
                if node_map.len() >= MAX_IMPACT_NODES {
                    break;
                }
                if let Ok(Some(sym_name)) = store.symbol_name_by_id(sym_id) {
                    if node_map.contains_key(&sym_name) {
                        continue;
                    }
                    let row: Option<(String, String, Option<i64>)> = conn
                        .query_row(
                            "SELECT f.path, s.kind, s.line_start \
                             FROM symbols s JOIN files f ON f.id = s.file_id \
                             WHERE s.id = ?1 LIMIT 1",
                            rusqlite::params![sym_id],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )
                        .ok();
                    let (file, kind, line) = row.unwrap_or_else(|| ("?".into(), "?".into(), None));
                    node_map.insert(
                        sym_name.clone(),
                        ImpactNode {
                            id: sym_name.clone(),
                            label: short_name(&sym_name),
                            file,
                            kind,
                            changed: false,
                            depth,
                            line: line.map(|l| l as u32),
                        },
                    );
                }
            }
        }
    }

    let direct_symbols = changed_symbols.len();
    let node_truncated = node_map.len() >= MAX_IMPACT_NODES;
    let impacted_symbols = node_map.len().saturating_sub(direct_symbols);
    let nodes: Vec<ImpactNode> = node_map.into_values().collect();

    // Prune edges whose endpoints were dropped when the node cap was hit;
    // d3.forceLink requires every link to resolve to a node in the set.
    let retained: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let edges: Vec<ImpactEdge> = edges
        .into_iter()
        .filter(|e| retained.contains(e.src.as_str()) && retained.contains(e.dst.as_str()))
        .collect();

    Ok(PrImpactGraph {
        pr_number: number,
        changed_files: changed_files.into_iter().collect(),
        nodes,
        edges,
        direct_symbols,
        impacted_symbols,
        truncated: file_truncated || node_truncated,
    })
}

const MAX_IMPACT_NODES: usize = 300;
const FILES_PAGE_LIMIT: usize = 3; // 3 × 100 = 300 files max

fn short_path(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn short_name(name: &str) -> String {
    // "a::b::c::d" → "c::d" (keep last two segments for readability)
    let parts: Vec<&str> = name.split("::").collect();
    if parts.len() <= 2 {
        name.to_string()
    } else {
        parts[parts.len() - 2..].join("::")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_repo_ssh() {
        assert_eq!(
            parse_github_repo("git@github.com:owner/repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            parse_github_repo("git@github.com:org/sub-repo.git"),
            Some("org/sub-repo".to_string())
        );
    }

    #[test]
    fn parse_github_repo_https() {
        assert_eq!(
            parse_github_repo("https://github.com/owner/repo"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            parse_github_repo("https://github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            parse_github_repo("http://github.com/owner/repo"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn parse_github_repo_non_github() {
        assert_eq!(parse_github_repo("https://gitlab.com/owner/repo"), None);
        assert_eq!(parse_github_repo("git@bitbucket.org:owner/repo.git"), None);
        assert_eq!(parse_github_repo(""), None);
        assert_eq!(parse_github_repo("https://github.com/no-slash"), None);
    }

    #[test]
    fn short_name_truncates_long_paths() {
        assert_eq!(short_name("a::b::c::d"), "c::d");
        assert_eq!(short_name("foo::bar"), "foo::bar");
        assert_eq!(short_name("single"), "single");
    }

    #[test]
    fn short_path_extracts_filename() {
        assert_eq!(short_path("src/foo/bar.rs"), "bar.rs");
        assert_eq!(short_path("bar.rs"), "bar.rs");
    }
}
