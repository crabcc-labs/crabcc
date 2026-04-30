//! `crabcc info --status-line` / `--is-repo` (issue #43).
//!
//! Render-budget-friendly status output for Starship / tmux / VS Code
//! status bars. Each component query is intentionally cheap:
//!
//! - **token savings**: `crabcc_core::track::report()` reads the
//!   `.crabcc/track.json` log — single file read, zero parse work
//!   beyond `serde_json`.
//! - **index freshness**: one `stat()` on `.crabcc/index.db` for the
//!   mtime; format the wall-clock delta.
//! - **memory drawers**: `Palace::open(...).count()` — one `SELECT
//!   COUNT(*) FROM drawers`. The `Palace` open is cheap because the
//!   PalaceRegistry caches connections by canonical git-root (issue
//!   #30).
//! - **Claude Code activity**: read the most recent `*.jsonl` under
//!   `~/.claude/projects/<encoded-cwd>/sessions/`, count tool_use
//!   events. Best-effort; absent when the user isn't in a CC session.
//!
//! Total target: p95 < 50ms on M-series Mac. Each component degrades
//! gracefully — a missing source means that segment is omitted, not an
//! error. Starship hides the module via `--is-repo` so the failure
//! mode for "not in a crabcc repo" is "no module rendered".

use anyhow::Result;
use crabcc_memory::Palace;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// True when `path/.crabcc/index.db` exists, OR walking up from `path`
/// finds an ancestor with one. Mirrors how `Palace::open` resolves
/// per-repo state.
pub fn is_repo(path: &Path) -> bool {
    locate_repo(path).is_some()
}

/// Walk up looking for the nearest `.crabcc/index.db`. Returns the
/// directory containing it (the canonical repo root for crabcc state).
pub fn locate_repo(start: &Path) -> Option<PathBuf> {
    let mut p = start.canonicalize().ok()?;
    loop {
        if p.join(".crabcc").join("index.db").exists() {
            return Some(p);
        }
        p = p.parent()?.to_path_buf();
    }
}

#[derive(Debug, Default)]
pub struct StatusReport {
    /// Cumulative tokens saved across all sessions, formatted as
    /// `"87.2k"` etc. Empty when no track log exists.
    pub saved_tokens: Option<String>,
    /// Wall-clock seconds since the last `crabcc index` / `refresh`.
    /// Formatted via `format_age` to `"12s"` / `"3m"` / `"2h"`.
    pub index_age: Option<String>,
    /// Drawer count in the per-repo memory store. Empty when the
    /// memory db hasn't been initialised yet.
    pub drawer_count: Option<String>,
    /// Claude Code session activity for the current repo: number of
    /// tool_use events seen in the latest session JSONL. Empty when
    /// the user isn't running CC, or when the projects/ dir is absent.
    pub cc_tools: Option<usize>,
    /// `cwd` resolved repo root (the directory carrying `.crabcc/`).
    pub root: Option<PathBuf>,
}

pub fn run_status_line(cwd: &Path, json: bool) -> Result<()> {
    let report = collect(cwd);
    if json {
        // Hand-rolled JSON via `json!` so `StatusReport` doesn't have
        // to carry a `serde` dep at the cli-crate boundary.
        let v = serde_json::json!({
            "saved_tokens":  report.saved_tokens,
            "index_age":     report.index_age,
            "drawer_count":  report.drawer_count,
            "cc_tools":      report.cc_tools,
            "root":          report.root.as_ref().and_then(|p| p.to_str()),
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        println!("{}", format_text(&report));
    }
    Ok(())
}

/// Collect the four signals. Best-effort: each block's `Result` is
/// discarded so a single missing source doesn't kill the whole report.
pub fn collect(cwd: &Path) -> StatusReport {
    let mut r = StatusReport::default();
    let root = match locate_repo(cwd) {
        Some(p) => p,
        None => return r,
    };
    r.root = Some(root.clone());

    // Token savings — single file read.
    if let Ok(rep) = crabcc_core::track::report() {
        r.saved_tokens = Some(format_compact(rep.all_time.saved_tokens));
    }

    // Index age via mtime stat.
    let db = root.join(".crabcc").join("index.db");
    if let Ok(meta) = std::fs::metadata(&db) {
        if let Ok(modified) = meta.modified() {
            if let Ok(elapsed) = SystemTime::now().duration_since(modified) {
                r.index_age = Some(format_age(elapsed));
            }
        }
    }

    // Drawer count via Palace. Wrapped so a missing memory db doesn't
    // cancel the whole report.
    if let Ok(palace) = Palace::open(&root) {
        if let Ok(n) = palace.count() {
            r.drawer_count = Some(format_compact(n));
        }
    }

    // Claude Code session activity. Best-effort glob + tail-parse.
    if let Some(n) = claude_code_tool_count(&root) {
        r.cc_tools = Some(n);
    }

    r
}

/// Format the report as `crabcc 87.2k · idx 12s · mem 1.4k · 4 tools`.
/// Compact by design: the segment positions imply meaning (tokens →
/// index age → memory drawers → CC tool calls), so no qualifier text
/// is needed. Segments are dropped when their source is unavailable
/// (graceful degradation; Starship hides the whole module when
/// `is_repo` is false so we should never render "crabcc" alone).
pub fn format_text(r: &StatusReport) -> String {
    let mut parts = Vec::with_capacity(4);
    if let Some(s) = &r.saved_tokens {
        parts.push(s.clone());
    }
    if let Some(a) = &r.index_age {
        parts.push(format!("idx {a}"));
    }
    if let Some(d) = &r.drawer_count {
        parts.push(format!("mem {d}"));
    }
    if let Some(t) = r.cc_tools {
        parts.push(format!("{t} tools"));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("crabcc {}", parts.join(" · "))
}

/// `87234` → `"87.2k"`; `1_400_000` → `"1.4M"`; `42` stays `"42"`.
/// Threshold 1000 to keep small numbers exact; one decimal place above.
pub fn format_compact(n: usize) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    if n < 1_000_000 {
        return format!("{:.1}k", n as f64 / 1_000.0);
    }
    if n < 1_000_000_000 {
        return format!("{:.1}M", n as f64 / 1_000_000.0);
    }
    format!("{:.1}G", n as f64 / 1_000_000_000.0)
}

/// `Duration` → terse "Ns" / "Nm" / "Nh" / "Nd". Truncates to the
/// largest stable unit so the status line doesn't twitch every second.
pub fn format_age(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        return format!("{s}s");
    }
    if s < 3_600 {
        return format!("{}m", s / 60);
    }
    if s < 86_400 {
        return format!("{}h", s / 3_600);
    }
    format!("{}d", s / 86_400)
}

/// Count `tool_use` events in the most recent CC session JSONL for
/// the given repo. Returns None when the projects dir is absent.
///
/// CC encodes the session path by replacing `/` with `-` after a
/// leading `-` (the absolute-path leading slash). Example:
///
///   /Users/foo/repo  →  ~/.claude/projects/-Users-foo-repo/sessions/...
fn claude_code_tool_count(root: &Path) -> Option<usize> {
    let home = std::env::var_os("HOME")?;
    let encoded = encode_repo_path(root)?;
    let sessions_dir = PathBuf::from(home)
        .join(".claude")
        .join("projects")
        .join(encoded)
        .join("sessions");
    if !sessions_dir.exists() {
        return None;
    }

    // Pick the most recently modified .jsonl.
    let latest = std::fs::read_dir(&sessions_dir).ok()?.flatten().fold(
        None::<(SystemTime, PathBuf)>,
        |best, entry| {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                return best;
            }
            let mtime = entry.metadata().and_then(|m| m.modified()).ok()?;
            match best {
                Some((b, _)) if b >= mtime => best,
                _ => Some((mtime, path)),
            }
        },
    )?;

    // Tally tool_use events. tail-parse: read whole file (sessions
    // are tiny — a few KB to a few MB) and count occurrences of
    // `"type":"tool_use"`. Substring count avoids JSON parse cost on
    // every render.
    let body = std::fs::read_to_string(&latest.1).ok()?;
    Some(body.matches("\"type\":\"tool_use\"").count())
}

fn encode_repo_path(root: &Path) -> Option<String> {
    // Drop the leading `/` then prepend a single `-`. Replace inner
    // `/` with `-`. Matches Claude Code's projects/ encoding.
    let s = root.to_str()?;
    if !s.starts_with('/') {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    out.push('-');
    out.push_str(&s[1..].replace('/', "-"));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn format_compact_thresholds() {
        assert_eq!(format_compact(0), "0");
        assert_eq!(format_compact(42), "42");
        assert_eq!(format_compact(999), "999");
        assert_eq!(format_compact(1_000), "1.0k");
        assert_eq!(format_compact(87_234), "87.2k");
        assert_eq!(format_compact(999_999), "1000.0k");
        assert_eq!(format_compact(1_400_000), "1.4M");
        assert_eq!(format_compact(2_000_000_000), "2.0G");
    }

    #[test]
    fn format_age_units() {
        use std::time::Duration;
        assert_eq!(format_age(Duration::from_secs(0)), "0s");
        assert_eq!(format_age(Duration::from_secs(59)), "59s");
        assert_eq!(format_age(Duration::from_secs(60)), "1m");
        assert_eq!(format_age(Duration::from_secs(3_599)), "59m");
        assert_eq!(format_age(Duration::from_secs(3_600)), "1h");
        assert_eq!(format_age(Duration::from_secs(86_399)), "23h");
        assert_eq!(format_age(Duration::from_secs(86_400)), "1d");
    }

    #[test]
    fn encode_repo_path_matches_cc_convention() {
        let p = Path::new("/Users/foo/repo");
        assert_eq!(encode_repo_path(p).unwrap(), "-Users-foo-repo");
    }

    #[test]
    fn encode_repo_path_rejects_relative() {
        let p = Path::new("foo/repo");
        assert!(encode_repo_path(p).is_none());
    }

    #[test]
    fn is_repo_false_outside_repo() {
        let dir = tempdir().unwrap();
        assert!(!is_repo(dir.path()));
    }

    #[test]
    fn is_repo_true_when_index_db_present() {
        let dir = tempdir().unwrap();
        let crabcc_dir = dir.path().join(".crabcc");
        std::fs::create_dir_all(&crabcc_dir).unwrap();
        std::fs::write(crabcc_dir.join("index.db"), b"").unwrap();
        assert!(is_repo(dir.path()));
    }

    #[test]
    fn is_repo_walks_up_from_subdir() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".crabcc")).unwrap();
        std::fs::write(dir.path().join(".crabcc").join("index.db"), b"").unwrap();
        let nested = dir.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(is_repo(&nested));
    }

    #[test]
    fn format_text_empty_report_is_empty_string() {
        assert_eq!(format_text(&StatusReport::default()), "");
    }

    #[test]
    fn format_text_combines_segments_with_dot() {
        let r = StatusReport {
            saved_tokens: Some("87.2k".into()),
            index_age: Some("12s".into()),
            drawer_count: Some("1.4k".into()),
            cc_tools: Some(4),
            ..Default::default()
        };
        assert_eq!(
            format_text(&r),
            "crabcc 87.2k · idx 12s · mem 1.4k · 4 tools"
        );
    }

    #[test]
    fn format_text_drops_missing_segments() {
        let r = StatusReport {
            saved_tokens: Some("12k".into()),
            index_age: None,
            drawer_count: Some("3".into()),
            cc_tools: None,
            ..Default::default()
        };
        assert_eq!(format_text(&r), "crabcc 12k · mem 3");
    }

    #[test]
    fn collect_returns_empty_outside_repo() {
        let dir = tempdir().unwrap();
        let r = collect(dir.path());
        assert!(r.root.is_none());
        assert!(r.saved_tokens.is_none());
        assert!(r.drawer_count.is_none());
    }

    #[test]
    fn collect_populates_index_age_when_db_present() {
        let dir = tempdir().unwrap();
        let crabcc_dir = dir.path().join(".crabcc");
        std::fs::create_dir_all(&crabcc_dir).unwrap();
        std::fs::write(crabcc_dir.join("index.db"), b"").unwrap();
        let r = collect(dir.path());
        assert!(r.root.is_some());
        assert!(
            r.index_age.is_some(),
            "expected index_age to be populated for fresh index.db"
        );
    }
}
