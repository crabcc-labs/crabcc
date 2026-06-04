//! Best-effort dev-debug ledger for the shell-rewrite hook, stored in
//! the singleton `~/.crabcc/_internal.db` (`rewrite_log` +
//! `rewrite_suppress` tables; schema lives in `agent_runs_db::open`).
//!
//! Two halves of the measure/learn loop:
//!   * **pre-exec** ([`log_event`], [`is_suppressed`]) — every emitted
//!     rewrite is logged; rewrites whose `(rule, key)` signature has been
//!     suppressed by a prior bad measurement are skipped by the caller.
//!   * **post-exec** ([`measure`]) — the PostToolUse hook feeds back the
//!     rewritten command's actual output size. A symbol upgrade that
//!     blew past its token budget (i.e. did not reduce tokens) is marked
//!     `META_ERROR_OPERATOR_NEEDED` and its signature is suppressed so it
//!     passes through next time.
//!
//! Everything here is best-effort: a locked/absent DB degrades to a
//! no-op. Logging must never break the user's command.

use crate::agent_runs_db;
use rusqlite::{params, Connection};
use std::path::PathBuf;

/// A symbol upgrade producing more than this many output tokens did not
/// actually save anything (the identifier is too common for `lookup
/// refs` to beat a scoped search) — flag + suppress it.
const SYMBOL_UPGRADE_BUDGET_TOKENS: i64 = 4000;

/// Keep roughly the last ~2MB of history (≈12k rows at ~160 B/row).
/// Pruned lazily (every 64th insert) so logging stays cheap.
const MAX_ROWS: i64 = 12_000;

/// The operator-facing marker the user asked for: a rewrite that turned
/// out not to reduce tokens.
pub const META_ERROR: &str = "META_ERROR_OPERATOR_NEEDED";

/// Open the singleton internal DB, best-effort. `None` if `$HOME` is
/// unset or the DB can't be opened — callers degrade to no-op.
pub fn open_internal() -> Option<Connection> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    agent_runs_db::open(&agent_runs_db::default_db_path(&home)).ok()
}

/// Suppression / log signature for a rewrite: `rule:key` (the key is the
/// symbol for upgrades, the pattern/glob for swaps). Suppressing one
/// symbol's upgrade must not disable every other rewrite.
pub fn signature(rule: &str, key: &str) -> String {
    format!("{rule}:{key}")
}

/// Has this signature been suppressed by a prior bad measurement?
pub fn is_suppressed(conn: &Connection, sig: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM rewrite_suppress WHERE sig = ?1",
        params![sig],
        |_| Ok(()),
    )
    .is_ok()
}

/// Log an emitted rewrite (pre-exec). Best-effort; errors swallowed.
#[allow(clippy::too_many_arguments)]
pub fn log_event(
    conn: &Connection,
    session: Option<&str>,
    rule: &str,
    sig: &str,
    original: &str,
    rewritten: &str,
    est_saved: i64,
) {
    let res = conn.execute(
        "INSERT INTO rewrite_log (ts, session, rule, sig, original, rewritten, est_saved) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![now_ts(), session, rule, sig, original, rewritten, est_saved],
    );
    if res.is_ok() {
        maybe_prune(conn);
    }
}

/// Post-exec feedback: given the command that actually ran (the
/// PostToolUse `tool_input.command`) and its output size in tokens,
/// attach the measurement to the matching log row and decide a verdict.
/// A symbol upgrade over budget is suppressed and flagged with
/// [`META_ERROR`]. Returns the verdict, or `None` if the command isn't a
/// (still-unmeasured) crabcc rewrite.
pub fn measure_by_command(
    conn: &Connection,
    command: &str,
    out_tokens: i64,
) -> Option<&'static str> {
    let row = conn
        .query_row(
            "SELECT id, rule FROM rewrite_log \
             WHERE rewritten = ?1 AND verdict IS NULL ORDER BY ts DESC LIMIT 1",
            params![command],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .ok()?;
    let (id, rule) = row;

    // Only symbol upgrades can "fail": a faithful rg/find swap is a
    // gitignore-filtered superset of the original, never worse on tokens.
    let over_budget = rule.ends_with("crabcc-refs") && out_tokens > SYMBOL_UPGRADE_BUDGET_TOKENS;
    let verdict = if over_budget { META_ERROR } else { "helped" };

    let _ = conn.execute(
        "UPDATE rewrite_log SET out_tokens = ?1, verdict = ?2 WHERE id = ?3",
        params![out_tokens, verdict, id],
    );
    if over_budget {
        let _ = conn.execute(
            "INSERT OR IGNORE INTO rewrite_suppress (sig, rule, since_ts, reason) \
             SELECT sig, rule, ?2, ?3 FROM rewrite_log WHERE id = ?1",
            params![
                id,
                now_ts(),
                format!("{out_tokens} output tokens > {SYMBOL_UPGRADE_BUDGET_TOKENS} budget")
            ],
        );
    }
    Some(verdict)
}

/// Lazily cap the table to ~`MAX_ROWS` (every 64th insert) so the ledger
/// stays around ~2MB without a size scan on every write.
fn maybe_prune(conn: &Connection) {
    let last = conn.last_insert_rowid();
    if last % 64 != 0 {
        return;
    }
    let _ = conn.execute(
        "DELETE FROM rewrite_log WHERE id <= (SELECT MAX(id) FROM rewrite_log) - ?1",
        params![MAX_ROWS],
    );
}

fn now_ts() -> i64 {
    crabcc_core::time::unix_now_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Drive the full loop: log a symbol upgrade, then feed back an
    /// over-budget measurement -> it is flagged META_ERROR and its
    /// signature becomes suppressed; a within-budget one stays clean.
    #[test]
    fn measure_suppresses_over_budget_symbol_upgrade() {
        let home = tempdir().unwrap();
        let db = agent_runs_db::default_db_path(home.path());
        let conn = agent_runs_db::open(&db).unwrap();

        let big = signature("grep->crabcc-refs", "Store");
        log_event(
            &conn,
            Some("s1"),
            "grep->crabcc-refs",
            &big,
            "grep -rn Store .",
            "crabcc lookup refs Store",
            2000,
        );
        assert!(
            !is_suppressed(&conn, &big),
            "not suppressed before measurement"
        );

        // Over budget -> META_ERROR + suppressed.
        assert_eq!(
            measure_by_command(&conn, "crabcc lookup refs Store", 9000),
            Some(META_ERROR)
        );
        assert!(
            is_suppressed(&conn, &big),
            "over-budget upgrade must be suppressed"
        );

        // A different, within-budget symbol stays clean.
        let small = signature("grep->crabcc-refs", "Rewrite");
        log_event(
            &conn,
            None,
            "grep->crabcc-refs",
            &small,
            "grep -rn Rewrite .",
            "crabcc lookup refs Rewrite",
            2000,
        );
        assert_eq!(
            measure_by_command(&conn, "crabcc lookup refs Rewrite", 300),
            Some("helped")
        );
        assert!(!is_suppressed(&conn, &small));

        // An unknown command (not a logged rewrite) measures to None.
        assert_eq!(measure_by_command(&conn, "ls -la", 10), None);
    }

    /// Faithful rg swaps are never auto-suppressed regardless of output
    /// size (they are gitignore-filtered supersets of grep, so they
    /// cannot be worse on tokens than the original).
    #[test]
    fn rg_swaps_are_never_suppressed() {
        let home = tempdir().unwrap();
        let db = agent_runs_db::default_db_path(home.path());
        let conn = agent_runs_db::open(&db).unwrap();
        let sig = signature("grep->rg", "TODO");
        log_event(
            &conn,
            None,
            "grep->rg",
            &sig,
            "grep -rn TODO .",
            "rg -n TODO",
            0,
        );
        assert_eq!(
            measure_by_command(&conn, "rg -n TODO", 999_999),
            Some("helped")
        );
        assert!(!is_suppressed(&conn, &sig));
    }
}
