// Usage tracking — estimates tokens saved per query and aggregates them.
//
// Storage: ~/.crabcc/usage.log (JSONL, append-only). One file globally so
// `crabcc track` can show savings across every repo without configuration.
//
// Saved-token math is rough by design — we don't know what the agent
// would actually have done in the absence of crabcc. Heuristics tuned
// for the common path (one grep + a handful of Read calls).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Entry {
    pub ts: u64,
    pub op: String,
    pub query: String,
    pub results: usize,
    pub repo: String,
    pub used_tokens: usize,
    pub saved_tokens: usize,
}

/// Estimate the tokens crabcc would have spent in the agent's context if
/// it had answered this question via raw grep + Read(s).
/// This is intentionally conservative — assumes the agent is efficient.
pub fn estimate_saved(op: &str, results: usize, used_tokens: usize) -> usize {
    let raw_estimate = match op {
        // grep finds the def, agent then Reads that file (~3-5k tokens).
        "sym"     => 3_500,
        // grep returns N hit lines (~2k for a big repo) plus selective file
        // Reads — assume agent Reads up to 30 unique files at ~1k tokens.
        "refs"    => 2_000 + (results.min(100) * 300),
        "callers" => 2_000 + (results.min(100) * 300),
        // outline replaces a full-file Read; large files get expensive.
        "outline" => 6_000,
        _ => 0,
    };
    raw_estimate.saturating_sub(used_tokens)
}

/// Approximate tokens from a JSON byte count. Token≈4 chars works for
/// LLM tokenizers within ±20% on JSON-shaped text.
pub fn tokens_for_bytes(bytes: usize) -> usize {
    bytes / 4
}

fn log_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let dir = home.join(".crabcc");
    let _ = fs::create_dir_all(&dir);
    Some(dir.join("usage.log"))
}

/// Append a usage record. Failures are swallowed — tracking must never
/// break the user's actual query.
pub fn record(op: &str, query: &str, results: usize, repo: &str, output_bytes: usize) {
    let used = tokens_for_bytes(output_bytes);
    let saved = estimate_saved(op, results, used);
    let entry = Entry {
        ts: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        op: op.into(),
        query: query.chars().take(200).collect(),
        results,
        repo: repo.into(),
        used_tokens: used,
        saved_tokens: saved,
    };
    let Some(path) = log_path() else { return };
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) else { return };
    if let Ok(line) = serde_json::to_string(&entry) {
        let _ = writeln!(f, "{line}");
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Bucket {
    pub queries: usize,
    pub used_tokens: usize,
    pub saved_tokens: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub session:    Bucket,   // last 30 min
    pub last_24h:   Bucket,
    pub all_time:   Bucket,
    pub by_op:      std::collections::BTreeMap<String, Bucket>,
}

pub fn read_log() -> Result<Vec<Entry>> {
    let Some(path) = log_path() else { return Ok(Vec::new()) };
    if !path.exists() { return Ok(Vec::new()); }
    let body = fs::read_to_string(&path)?;
    let mut out = Vec::new();
    for line in body.lines() {
        if line.trim().is_empty() { continue; }
        if let Ok(e) = serde_json::from_str::<Entry>(line) {
            out.push(e);
        }
    }
    Ok(out)
}

pub fn report() -> Result<Report> {
    let entries = read_log()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let session_cutoff = now.saturating_sub(30 * 60);
    let day_cutoff     = now.saturating_sub(24 * 60 * 60);

    let mut r = Report::default();
    for e in &entries {
        let bumps = [&mut r.all_time];
        for b in bumps { add(b, e); }
        if e.ts >= day_cutoff { add(&mut r.last_24h, e); }
        if e.ts >= session_cutoff { add(&mut r.session, e); }
        let op_b = r.by_op.entry(e.op.clone()).or_default();
        add(op_b, e);
    }
    Ok(r)
}

fn add(b: &mut Bucket, e: &Entry) {
    b.queries += 1;
    b.used_tokens += e.used_tokens;
    b.saved_tokens += e.saved_tokens;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_saved_known_ops() {
        assert!(estimate_saved("sym", 1, 100) > 0);
        assert!(estimate_saved("refs", 50, 1000) > estimate_saved("refs", 1, 1000));
        assert!(estimate_saved("callers", 0, 99_999) == 0,
                "saved is non-negative when we use more than the raw estimate");
        assert_eq!(estimate_saved("frobnicate", 1, 1), 0);
    }

    #[test]
    fn refs_caps_at_100_results() {
        let a = estimate_saved("refs", 100,   0);
        let b = estimate_saved("refs", 5_000, 0);
        assert_eq!(a, b, "should cap at 100 results to avoid wild claims");
    }

    #[test]
    fn tokens_for_bytes_small_value() {
        assert_eq!(tokens_for_bytes(0),  0);
        assert_eq!(tokens_for_bytes(40), 10);
    }

    #[test]
    fn record_then_report_roundtrip() {
        // Use a private log under HOME=tempdir so we don't pollute the user's log.
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        // SAFETY: tests in this binary aren't run multi-threaded against this var
        // on macOS by default for cargo test (parallel by file but not across HOME).
        std::env::set_var("HOME", dir.path());
        record("sym", "User", 3, "test_repo", 200);
        record("refs", "Foo", 20, "test_repo", 1200);
        let r = report().unwrap();
        assert_eq!(r.all_time.queries, 2);
        assert!(r.all_time.saved_tokens > 0);
        assert!(r.by_op.contains_key("sym"));
        assert!(r.by_op.contains_key("refs"));
        if let Some(prev) = prev { std::env::set_var("HOME", prev); }
    }
}
