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
use std::sync::Mutex;
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
    /// Agent run id when this record originated inside an agent
    /// process. `None` for direct CLI / IDE / MCP calls. Populated
    /// from a process-global setter ([`set_active_agent_id`]) — the
    /// agent runtime installs its `RunDir::id` once on startup;
    /// every subsequent `record` in that process picks it up. Added
    /// in #311 so the dashboard's Timeline route can group
    /// consecutive same-agent rows. Older log lines (without the
    /// field) decode as `None` thanks to `serde(default)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// Process-global agent run id. Set once per process by the agent
/// runtime via [`set_active_agent_id`]; read by [`record`] on every
/// log append. `Mutex<Option<String>>` rather than `OnceLock` so a
/// long-running test harness (or a misbehaving caller) can clear /
/// replace it without restarting the process.
static ACTIVE_AGENT_ID: Mutex<Option<String>> = Mutex::new(None);

/// Stamp every subsequent [`record`] call in this process with the
/// given agent run id. Idempotent — calling twice with different
/// values just replaces the prior one. Called once by the agent
/// runtime after `RunDir::create` in `crabcc-cli::agent::run`. Pass
/// `None` to clear (e.g. in tests).
pub fn set_active_agent_id(id: Option<String>) {
    if let Ok(mut guard) = ACTIVE_AGENT_ID.lock() {
        *guard = id;
    }
}

fn current_agent_id() -> Option<String> {
    ACTIVE_AGENT_ID.lock().ok().and_then(|g| g.clone())
}

/// Estimate the tokens crabcc would have spent in the agent's context if
/// it had answered this question via raw grep + Read(s).
/// This is intentionally conservative — assumes the agent is efficient.
pub fn estimate_saved(op: &str, results: usize, used_tokens: usize) -> usize {
    let raw_estimate = match op {
        // grep finds the def, agent then Reads that file (~3-5k tokens).
        "sym" => 3_500,
        // grep returns N hit lines (~2k for a big repo) plus selective file
        // Reads — assume agent Reads up to 30 unique files at ~1k tokens.
        "refs" => 2_000 + (results.min(100) * 300),
        "callers" => 2_000 + (results.min(100) * 300),
        // outline replaces a full-file Read; large files get expensive.
        "outline" => 6_000,
        // Fuzzy / prefix replace either nothing-the-agent-could-have-done
        // or a brittle regex sweep; conservative flat estimate.
        "fuzzy" => 2_500,
        "prefix" => 1_500,
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
    record_saved(op, query, results, repo, used, saved);
}

/// Like [`record`] but with an explicit, already-measured `saved_tokens`
/// (e.g. Morph compaction, where we know real input-vs-output bytes
/// rather than estimating from an op heuristic). Best-effort append.
pub fn record_saved(
    op: &str,
    query: &str,
    results: usize,
    repo: &str,
    used_tokens: usize,
    saved_tokens: usize,
) {
    let entry = Entry {
        ts: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default(),
        op: op.into(),
        query: query.chars().take(200).collect(),
        results,
        repo: repo.into(),
        used_tokens,
        saved_tokens,
        agent_id: current_agent_id(),
    };
    let Some(path) = log_path() else { return };
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
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
    pub session: Bucket, // last 30 min
    pub last_24h: Bucket,
    pub all_time: Bucket,
    pub by_op: std::collections::BTreeMap<String, Bucket>,
}

pub fn read_log() -> Result<Vec<Entry>> {
    let Some(path) = log_path() else {
        return Ok(Vec::new());
    };
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body = fs::read_to_string(&path)?;
    let out = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
        .collect();
    Ok(out)
}

pub fn report() -> Result<Report> {
    let entries = read_log()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let session_cutoff = now.saturating_sub(30 * 60);
    let day_cutoff = now.saturating_sub(24 * 60 * 60);

    let mut r = Report::default();
    for e in &entries {
        add(&mut r.all_time, e);
        if e.ts >= day_cutoff {
            add(&mut r.last_24h, e);
        }
        if e.ts >= session_cutoff {
            add(&mut r.session, e);
        }
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
    use std::sync::Mutex;

    // Track tests mutate the process-wide $HOME to redirect ~/.crabcc/usage.log
    // into a tempdir. cargo runs tests in parallel by default, and parallel
    // mutation of $HOME means concurrent track-tests stomp each other's logs.
    // Serialize them.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    fn with_isolated_home<F: FnOnce()>(f: F) {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        if let Some(prev) = prev {
            std::env::set_var("HOME", prev);
        } else {
            std::env::remove_var("HOME");
        }
        if let Err(e) = r {
            std::panic::resume_unwind(e);
        }
        // dir is dropped after, which is fine — log isn't read once HOME flips back.
    }

    #[test]
    fn estimate_saved_known_ops() {
        assert!(estimate_saved("sym", 1, 100) > 0);
        assert!(estimate_saved("refs", 50, 1000) > estimate_saved("refs", 1, 1000));
        assert!(
            estimate_saved("callers", 0, 99_999) == 0,
            "saved is non-negative when we use more than the raw estimate"
        );
        assert_eq!(estimate_saved("frobnicate", 1, 1), 0);
    }

    #[test]
    fn refs_caps_at_100_results() {
        let a = estimate_saved("refs", 100, 0);
        let b = estimate_saved("refs", 5_000, 0);
        assert_eq!(a, b, "should cap at 100 results to avoid wild claims");
    }

    #[test]
    fn tokens_for_bytes_small_value() {
        assert_eq!(tokens_for_bytes(0), 0);
        assert_eq!(tokens_for_bytes(40), 10);
    }

    #[test]
    fn record_then_report_roundtrip() {
        with_isolated_home(|| {
            record("sym", "User", 3, "test_repo", 200);
            record("refs", "Foo", 20, "test_repo", 1200);
            let r = report().unwrap();
            assert_eq!(r.all_time.queries, 2);
            assert!(r.all_time.saved_tokens > 0);
            assert!(r.by_op.contains_key("sym"));
            assert!(r.by_op.contains_key("refs"));
        });
    }

    #[test]
    fn read_log_skips_malformed_lines() {
        with_isolated_home(|| {
            // Resolve the canonical log path via the same code path the lib uses,
            // then write directly to it.
            let p = log_path().expect("log_path");
            std::fs::write(
                &p,
                concat!(
                    "{\"ts\":1,\"op\":\"sym\",\"query\":\"X\",\"results\":1,",
                    "\"repo\":\"r\",\"used_tokens\":10,\"saved_tokens\":100}\n",
                    "this is not json\n",
                    "{not even close to a valid Entry}\n",
                    "\n",
                ),
            )
            .unwrap();
            let entries = read_log().unwrap();
            assert_eq!(entries.len(), 1, "malformed lines must be skipped");
            assert_eq!(entries[0].op, "sym");
        });
    }

    #[test]
    fn report_session_window_excludes_old_entries() {
        with_isolated_home(|| {
            let p = log_path().expect("log_path");
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let two_h_ago = now - 2 * 3600;
            let ten_s_ago = now - 10;
            std::fs::write(&p, format!(
                "{{\"ts\":{two_h_ago},\"op\":\"sym\",\"query\":\"old\",\"results\":1,\"repo\":\"r\",\"used_tokens\":1,\"saved_tokens\":1}}\n\
                 {{\"ts\":{ten_s_ago},\"op\":\"sym\",\"query\":\"new\",\"results\":1,\"repo\":\"r\",\"used_tokens\":1,\"saved_tokens\":1}}\n"
            )).unwrap();
            let r = report().unwrap();
            assert_eq!(r.all_time.queries, 2, "all_time should see both");
            assert_eq!(r.last_24h.queries, 2, "last_24h should see both");
            assert_eq!(
                r.session.queries, 1,
                "session window is 30 min — only the recent one"
            );
        });
    }

    #[test]
    fn estimate_saved_caps_at_results_100_for_callers() {
        let a = estimate_saved("callers", 100, 0);
        let b = estimate_saved("callers", 5_000, 0);
        assert_eq!(a, b, "callers should cap at 100 results, same as refs");
    }

    #[test]
    fn tokens_for_bytes_large_value() {
        // 4 KB → 1024 tokens.
        assert_eq!(tokens_for_bytes(4096), 1024);
        // 1 MB → 262144 tokens (rough: 1_048_576 / 4).
        assert_eq!(tokens_for_bytes(1_048_576), 262_144);
    }

    #[test]
    fn query_string_is_truncated_to_200_chars() {
        // A 300-char query must be stored as ≤200 chars.
        with_isolated_home(|| {
            let long_query = "q".repeat(300);
            record("sym", &long_query, 1, "r", 100);
            let entries = read_log().unwrap();
            assert_eq!(entries.len(), 1);
            assert!(
                entries[0].query.chars().count() <= 200,
                "query not truncated: {} chars",
                entries[0].query.chars().count()
            );
        });
    }

    #[test]
    fn report_by_op_covers_all_recorded_ops() {
        with_isolated_home(|| {
            record("outline", "Q", 0, "r", 500);
            record("fuzzy", "Q", 0, "r", 200);
            record("prefix", "Q", 0, "r", 100);
            let r = report().unwrap();
            assert!(r.by_op.contains_key("outline"));
            assert!(r.by_op.contains_key("fuzzy"));
            assert!(r.by_op.contains_key("prefix"));
            assert_eq!(r.all_time.queries, 3);
        });
    }

    #[test]
    fn estimate_saved_outline_and_prefix() {
        // outline > prefix: outline replaces a whole-file read.
        let outline_saved = estimate_saved("outline", 0, 0);
        let prefix_saved = estimate_saved("prefix", 0, 0);
        assert!(
            outline_saved > prefix_saved,
            "outline ({outline_saved}) should save more than prefix ({prefix_saved})"
        );
    }
}
