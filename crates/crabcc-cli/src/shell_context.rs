//! Smart standing-context injection for the SessionStart hook
//! (`crabcc shell context`). **On by default** — opt out with
//! `CRABCC_NO_CTX_INJECT=1`.
//!
//! Emits a SessionStart `hookSpecificOutput.additionalContext` envelope.
//! When the repo is indexed it is *smart*: it surfaces the most-
//! referenced symbols and densest files (from the index) plus a concrete
//! `crabcc lookup` example, so the agent reaches for crabcc + context7
//! instead of grep/cat. SessionStart is the documented low-cost
//! injection point (once per session, not per turn/tool).
//!
//! Bulletproof by design: any failure (no index, locked DB, query error)
//! logs a warning + OTEL event and falls back to the static reminders —
//! it never breaks the session. Override the whole text per-repo via
//! `.crabcc/ctx-inject.md`.

use anyhow::Result;
use rusqlite::Connection;
use std::fmt::Write as _;
use std::path::Path;

/// Static reminders used when the repo isn't indexed (or a query fails).
const STATIC_CONTEXT: &str = "\
crabcc standing context:
- Docs: for current library / API / framework docs, query the context7 MCP \
(resolve-library-id then query-docs) instead of relying on training data.
- Navigation: prefer crabcc (lookup sym/refs/callers, outline, read) over \
grep/find/cat -- symbol-aware and far cheaper on tokens. The crabcc MCP \
(mcp__crabcc__*) exposes the same surface.
- Follow-ups: when you discover a bug or a concrete enhancement during this \
work, open a GitHub issue (gh issue create) so it is not lost.";

/// Injection is on unless explicitly disabled.
pub fn enabled() -> bool {
    !truthy_env("CRABCC_NO_CTX_INJECT")
}

fn truthy_env(key: &str) -> bool {
    std::env::var(key)
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// Resolve the reminder text: repo override > smart index-derived >
/// static fallback. Never errors.
fn context_text(root: &Path, db: &Path) -> String {
    if let Some(custom) = repo_override(root) {
        return custom;
    }
    match smart_context(db) {
        Ok(Some(text)) => text,
        Ok(None) => STATIC_CONTEXT.to_string(),
        Err(e) => {
            // J: log + OTEL, then continue with the safe default.
            tracing::warn!(
                target: "crabcc::shell::context",
                error = %e,
                "ctx-inject: index query failed; using static reminders"
            );
            STATIC_CONTEXT.to_string()
        }
    }
}

/// Per-repo override at `.crabcc/ctx-inject.md`.
fn repo_override(root: &Path) -> Option<String> {
    let s = std::fs::read_to_string(root.join(".crabcc").join("ctx-inject.md")).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Build the index-derived "smart" context, or `Ok(None)` when there is
/// no usable index yet.
fn smart_context(db: &Path) -> Result<Option<String>> {
    if !db.exists() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let symbols = popular_symbols(&conn, 6)?;
    let files = dense_files(&conn, 4)?;
    if symbols.is_empty() && files.is_empty() {
        return Ok(None); // index exists but is empty -> static
    }

    let mut s = String::from(
        "crabcc has indexed this repo -- use it instead of grep/find/cat \
         (symbol-aware, ~10-100x fewer tokens). The crabcc MCP (mcp__crabcc__*) \
         exposes the same surface.\n",
    );
    if let Some((top, _)) = symbols.first() {
        let list = symbols
            .iter()
            .map(|(n, c)| format!("{n} ({c} refs)"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            s,
            "- Most-referenced symbols: {list}. Example: `crabcc lookup refs {top}` \
             (or the mcp__crabcc__refs tool)."
        );
    }
    if !files.is_empty() {
        let list = files
            .iter()
            .map(|(p, c)| format!("{p} ({c})"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(s, "- Densest files (symbol count): {list}.");
    }
    s.push_str(
        "- Docs: query the context7 MCP (resolve-library-id then query-docs) for \
         current library/API docs rather than training data.\n\
         - Follow-ups: open a GitHub issue (gh issue create) for bugs / enhancements \
         you discover during this work.",
    );
    Ok(Some(s))
}

/// Top symbols by incoming reference (edge) count, excluding unresolved
/// sentinels.
fn popular_symbols(conn: &Connection, limit: usize) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT s.name, COUNT(*) AS c \
         FROM edges e JOIN symbols s ON s.id = e.dst_symbol_id \
         WHERE s.kind != 'sentinel' \
         GROUP BY e.dst_symbol_id ORDER BY c DESC, s.name LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Files with the most symbols (good entry points to read).
fn dense_files(conn: &Connection, limit: usize) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT f.path, COUNT(*) AS c \
         FROM symbols s JOIN files f ON f.id = s.file_id \
         WHERE s.kind != 'sentinel' \
         GROUP BY s.file_id ORDER BY c DESC, f.path LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Print the SessionStart additionalContext envelope unless disabled.
pub fn run(root: &Path, db: &Path, _session_id: Option<&str>) -> Result<()> {
    if !enabled() {
        return Ok(());
    }
    let ctx = context_text(root, db);
    tracing::info!(
        target: "crabcc::shell::context",
        bytes = ctx.len(),
        "injected session context"
    );
    let out = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": ctx,
        }
    });
    println!("{out}");
    Ok(())
}
