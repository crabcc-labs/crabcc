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
use crabcc_memory::{DeleteSel, Palace, SearchMode};
use std::path::Path;

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
    /// Drawer count.
    Count,
    /// Health snapshot.
    Health,
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
        MemoryCmd::Count => {
            let n = palace.count()?;
            println!("{}", serde_json::json!({"count": n}));
        }
        MemoryCmd::Health => {
            println!("{}", sonic_rs::to_string(&palace.health())?);
        }
    }
    Ok(())
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
}
