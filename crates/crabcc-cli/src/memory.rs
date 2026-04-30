//! `crabcc memory ...` subcommand dispatch + auto-capture hook.
//!
//! Memory commands open `<root>/.crabcc/memory.db` directly via `Palace::open`
//! — the symbol-index `Store` is not loaded for these calls.
//!
//! `auto_capture` is a best-effort hook called from existing query commands
//! (sym/refs/callers/fuzzy/prefix). Gated by `CRABCC_AUTO_MEMORY=1` — zero
//! overhead when unset. Never fails the user-facing command on memory errors.

use anyhow::{anyhow, Result};
use clap::Subcommand;
use crabcc_memory::{
    mine::{project::MineProjectOpts, sessions::MineSessionsOpts},
    DeleteSel, Palace, SearchMode, DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_PAIR_BYTES,
};
use std::path::{Path, PathBuf};

#[derive(Subcommand, Debug)]
pub enum MemoryCmd {
    /// Create or reuse `.crabcc/memory.db`. Idempotent.
    Init,
    /// Store one drawer (manual; bulk mining lands in M1).
    Remember {
        /// Source identifier (file path, conversation id, free string).
        source: String,
        /// Body content.
        body: String,
        #[arg(long, default_value = "default")]
        wing: String,
        #[arg(long)]
        room: Option<String>,
    },
    /// Search top-K drawers. Default mode is `hybrid` (BM25 + vector,
    /// fused via Reciprocal Rank Fusion). `--mode` selects an ablation:
    /// `hybrid` (default), `lexical` (BM25 only), or `vector` (KNN only).
    Search {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        wing: Option<String>,
        #[arg(long)]
        room: Option<String>,
        #[arg(long, default_value = "hybrid")]
        mode: String,
    },
    /// Fetch one drawer verbatim by id.
    Get { id: i64 },
    /// List drawers (no similarity).
    List {
        #[arg(long)]
        wing: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Delete drawers. Specify exactly one of --id, --source, --all.
    Delete {
        #[arg(long)]
        id: Option<i64>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// `forget` is `delete` + `VACUUM` — rows disappear AND the on-disk
    /// `.crabcc/memory.db` shrinks. Use it to reclaim space; reach for
    /// `delete` for transient delete-then-reinsert flows.
    ///
    /// Specify exactly one of:
    ///   - `--drawer ID` — drop a single drawer by id
    ///   - `--wing W --before DATE` — drop everything in wing W with
    ///     `created_at < DATE` (DATE = RFC3339 or epoch seconds)
    ///
    /// Idempotent: missing IDs / empty wings / no-rows-in-window all
    /// return `{"forgotten": 0}` and still run VACUUM.
    Forget {
        /// Drawer id to forget (may be passed alone).
        #[arg(long)]
        drawer: Option<i64>,
        /// Wing name. Required when `--before` is set.
        #[arg(long)]
        wing: Option<String>,
        /// Cutoff timestamp — drawers with `created_at < before` are
        /// removed. Accepts RFC3339 (e.g. `2026-01-15T00:00:00Z`) or a
        /// bare epoch-seconds integer.
        #[arg(long)]
        before: Option<String>,
    },
    /// Drawer count.
    Count,
    /// Health snapshot.
    Health,
    /// Bulk-ingest drawers (M2). Idempotent — re-running emits zero new
    /// drawers when nothing changed.
    Mine {
        #[command(subcommand)]
        kind: MineKind,
    },
}

#[derive(Subcommand, Debug)]
pub enum MineKind {
    /// Walk a repo, store one drawer per text file under `wing="proj"`.
    /// `[PATH]` defaults to the memory-store root.
    Project {
        path: Option<PathBuf>,
        /// Per-file body cap in bytes; larger files are skipped.
        #[arg(long, default_value_t = DEFAULT_MAX_FILE_BYTES)]
        max_bytes: u64,
    },
    /// Walk a JSONL directory of Claude Code transcripts and store one
    /// drawer per `(user, assistant)` turn pair under `wing="session"`.
    /// `[DIR]` defaults to `$HOME/.claude/projects/`.
    Sessions {
        dir: Option<PathBuf>,
        /// Per-pair body cap in bytes; longer pairs are truncated.
        #[arg(long, default_value_t = DEFAULT_MAX_PAIR_BYTES)]
        max_pair_bytes: usize,
    },
}

fn parse_before_timestamp(raw: &str) -> Result<i64> {
    // Try epoch seconds first; fall back to RFC3339. We intentionally
    // accept both because `crabcc memory forget --before 1735689600` is
    // a natural shape for scripts and `--before 2025-01-01T00:00:00Z`
    // is more readable for humans.
    if let Ok(secs) = raw.parse::<i64>() {
        return Ok(secs);
    }
    // Minimal RFC3339 parse without bringing in chrono — the schema
    // stores `created_at` as INTEGER seconds, so we only need to map a
    // user-typed timestamp into that integer space.
    let parsed = time_parse_rfc3339(raw)
        .ok_or_else(|| anyhow!("--before must be epoch seconds or RFC3339, got {raw:?}"))?;
    Ok(parsed)
}

/// Tiny RFC3339 parser (no chrono dep). Handles `YYYY-MM-DDTHH:MM:SSZ`
/// and `YYYY-MM-DDTHH:MM:SS+00:00` shapes — enough for `--before`.
/// Returns None on anything weirder; the caller turns that into a clap
/// error message.
fn time_parse_rfc3339(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let minute: i64 = s.get(14..16)?.parse().ok()?;
    let second: i64 = s.get(17..19)?.parse().ok()?;
    // Days-from-epoch via the proleptic Gregorian "Howard Hinnant"
    // algorithm — works for any year, no leap-year bookkeeping in the
    // call site.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

pub fn run(root: &Path, cmd: MemoryCmd) -> Result<()> {
    let palace = Palace::open(root)?;
    match cmd {
        MemoryCmd::Init => {
            // Palace::open already created/reused the file; emit a JSON ack.
            let body = serde_json::json!({"status": "ok", "root": root.display().to_string()});
            println!("{body}");
        }
        MemoryCmd::Remember {
            source,
            body,
            wing,
            room,
        } => {
            let session = std::env::var("TERM_SESSION_ID").ok();
            let id = palace.remember_in_session(
                &wing,
                room.as_deref(),
                &source,
                &body,
                session.as_deref(),
            )?;
            println!("{}", serde_json::json!({"id": id}));
        }
        MemoryCmd::Search {
            query,
            limit,
            wing,
            room,
            mode,
        } => {
            let parsed = SearchMode::parse(&mode).ok_or_else(|| {
                anyhow!("invalid --mode {mode:?}; expected hybrid|lexical|vector")
            })?;
            let r =
                palace.search_with_mode(parsed, &query, limit, wing.as_deref(), room.as_deref())?;
            println!("{}", sonic_rs::to_string(&r)?);
        }
        MemoryCmd::Get { id } => match palace.get(id)? {
            Some(d) => println!("{}", sonic_rs::to_string(&d)?),
            None => println!("null"),
        },
        MemoryCmd::List { wing, limit } => {
            let drawers = palace.list_drawers(wing.as_deref(), limit)?;
            println!("{}", sonic_rs::to_string(&drawers)?);
        }
        MemoryCmd::Delete { id, source, all } => {
            let count = [id.is_some(), source.is_some(), all]
                .iter()
                .filter(|x| **x)
                .count();
            if count != 1 {
                anyhow::bail!("specify exactly one of --id, --source, --all");
            }
            let sel = if all {
                DeleteSel::All
            } else if let Some(i) = id {
                DeleteSel::ById(vec![i])
            } else {
                DeleteSel::BySource(source.unwrap())
            };
            let n = palace.delete(&sel)?;
            println!("{}", serde_json::json!({"deleted": n}));
        }
        MemoryCmd::Forget {
            drawer,
            wing,
            before,
        } => {
            // Two valid shapes:
            //   --drawer ID                       (drawer + no other flags)
            //   --wing W --before DATE            (both required; drawer absent)
            let sel = match (drawer, wing.as_deref(), before.as_deref()) {
                (Some(id), None, None) => DeleteSel::ById(vec![id]),
                (None, Some(w), Some(b)) => DeleteSel::BeforeInWing {
                    wing: w.to_string(),
                    before: parse_before_timestamp(b)?,
                },
                _ => anyhow::bail!(
                    "specify either `--drawer ID` or `--wing W --before DATE` (mutually exclusive)"
                ),
            };
            let n = palace.forget(&sel)?;
            println!("{}", serde_json::json!({"forgotten": n}));
        }
        MemoryCmd::Count => {
            let n = palace.count()?;
            println!("{}", serde_json::json!({"count": n}));
        }
        MemoryCmd::Health => {
            println!("{}", sonic_rs::to_string(&palace.health())?);
        }
        MemoryCmd::Mine { kind } => {
            let session = std::env::var("TERM_SESSION_ID").ok();
            let report = match kind {
                MineKind::Project { path, max_bytes } => {
                    let target = path.unwrap_or_else(|| root.to_path_buf());
                    let opts = MineProjectOpts {
                        max_bytes,
                        session_id: session,
                    };
                    palace.mine_project(&target, &opts)?
                }
                MineKind::Sessions {
                    dir,
                    max_pair_bytes,
                } => {
                    let target = dir.unwrap_or_else(default_sessions_dir);
                    let opts = MineSessionsOpts {
                        max_pair_bytes,
                        session_id: session,
                    };
                    palace.mine_sessions(&target, &opts)?
                }
            };
            println!("{}", sonic_rs::to_string(&report)?);
        }
    }
    Ok(())
}

/// `~/.claude/projects/` — the default home for Claude Code's per-repo
/// JSONL transcripts. Falls back to the current working directory if
/// `$HOME` isn't set (e.g. CI containers without a passwd entry).
fn default_sessions_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".claude").join("projects");
    }
    PathBuf::from(".")
}

/// Best-effort auto-capture for query-shaped commands. Off unless
/// `CRABCC_AUTO_MEMORY=1`. Errors are swallowed by design — capture is
/// secondary to the user-facing operation.
pub fn auto_capture(root: &Path, op: &str, query: &str, count: usize) {
    if !env_auto_capture_enabled() {
        return;
    }
    let session = std::env::var("TERM_SESSION_ID").ok();
    auto_capture_inner(root, op, query, count, session.as_deref());
}

/// Pure variant — no env reads. Used by tests and any caller wanting to
/// drive capture without the env-var gate.
pub fn auto_capture_inner(root: &Path, op: &str, query: &str, count: usize, session: Option<&str>) {
    let _: Result<()> = (|| {
        let palace = Palace::open(root)?;
        let body = format!("{op} \"{query}\" -> {count} hit(s)");
        // Source key includes op + query so re-asking the same thing dedups
        // (UNIQUE on (source_id, sha256) — body changes when count changes).
        palace.remember_in_session(
            "default",
            Some(op),
            &format!("query:{op}:{query}"),
            &body,
            session,
        )?;
        Ok(())
    })();
}

pub fn env_auto_capture_enabled() -> bool {
    std::env::var("CRABCC_AUTO_MEMORY").ok().as_deref() == Some("1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn auto_capture_inner_creates_drawer_with_session() {
        let dir = tempdir().unwrap();
        auto_capture_inner(dir.path(), "sym", "Foo", 3, Some("term:t1"));
        let palace = Palace::open(dir.path()).unwrap();
        let drawers = palace.list_drawers(None, 10).unwrap();
        assert_eq!(drawers.len(), 1);
        assert_eq!(drawers[0].source_id, "query:sym:Foo");
        assert_eq!(drawers[0].room.as_deref(), Some("sym"));
        assert_eq!(drawers[0].session_id.as_deref(), Some("term:t1"));
        assert!(drawers[0].body.contains("3 hit(s)"));
    }

    #[test]
    fn auto_capture_inner_works_without_session() {
        let dir = tempdir().unwrap();
        auto_capture_inner(dir.path(), "callers", "bar", 0, None);
        let palace = Palace::open(dir.path()).unwrap();
        let drawers = palace.list_drawers(None, 10).unwrap();
        assert_eq!(drawers.len(), 1);
        assert!(drawers[0].session_id.is_none());
    }

    #[test]
    fn auto_capture_inner_dedups_repeat_for_same_count() {
        // Same op + query + count → same sha → dedup'd. Two calls = one row.
        let dir = tempdir().unwrap();
        auto_capture_inner(dir.path(), "sym", "X", 2, None);
        auto_capture_inner(dir.path(), "sym", "X", 2, None);
        let palace = Palace::open(dir.path()).unwrap();
        assert_eq!(palace.count().unwrap(), 1);
    }

    #[test]
    fn auto_capture_inner_separates_drawers_when_count_changes() {
        // Same op + query but different count → different body → different
        // sha → new drawer with the same source_id.
        let dir = tempdir().unwrap();
        auto_capture_inner(dir.path(), "sym", "X", 1, None);
        auto_capture_inner(dir.path(), "sym", "X", 5, None);
        let palace = Palace::open(dir.path()).unwrap();
        assert_eq!(palace.count().unwrap(), 2);
    }

    // ---- forget --before parsing (issue #26) -------------------------------

    #[test]
    fn parse_before_timestamp_accepts_epoch_seconds() {
        // Bare integer → returned verbatim. Common shape for scripts that
        // build the cutoff via `date +%s`.
        let n = parse_before_timestamp("1700000000").unwrap();
        assert_eq!(n, 1_700_000_000);
    }

    #[test]
    fn parse_before_timestamp_accepts_rfc3339_z() {
        // 2025-01-01T00:00:00Z is epoch 1735689600.
        let n = parse_before_timestamp("2025-01-01T00:00:00Z").unwrap();
        assert_eq!(n, 1_735_689_600);
    }

    #[test]
    fn parse_before_timestamp_rejects_garbage() {
        // Anything that's not a bare integer or a recognisable RFC3339
        // shape must surface as an error so the CLI can show a usage
        // message rather than silently using `0` (which would forget
        // everything).
        assert!(parse_before_timestamp("yesterday").is_err());
        assert!(parse_before_timestamp("").is_err());
        assert!(parse_before_timestamp("2025/01/01").is_err());
    }
}
