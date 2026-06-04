//! Experimental standing-context injection for the SessionStart hook
//! (`crabcc shell context`). Off by default; enabled with the
//! `--exp-ctx-inject` flag or `CRABCC_EXP_CTX_INJECT=1`.
//!
//! When enabled it prints a SessionStart `hookSpecificOutput.
//! additionalContext` envelope carrying a few high-value standing
//! reminders. SessionStart is the documented low-cost injection point:
//! it fires once per session (and on resume/clear/compact), so the
//! reminders cost tokens once rather than per turn (UserPromptSubmit) or
//! per tool call (PreToolUse). When disabled it prints nothing, so the
//! SessionStart hook that calls it is a safe no-op until opted in.
//!
//! The reminder text is overridable per-repo via `.crabcc/ctx-inject.md`;
//! otherwise the built-in default below is used.

use anyhow::Result;
use std::path::Path;

/// Built-in standing reminders. Kept short — this is injected verbatim.
const DEFAULT_CONTEXT: &str = "\
crabcc standing context (experimental; CRABCC_EXP_CTX_INJECT):
- Docs: for current library / API / framework docs, query the context7 MCP \
(resolve-library-id then query-docs) instead of relying on training data.
- Navigation: prefer crabcc (lookup sym/refs/callers, outline, read) over \
grep/find/cat -- symbol-aware and far cheaper on tokens.
- Follow-ups: when you discover a bug or a concrete enhancement during this \
work, open a GitHub issue (gh issue create) so it is not lost.";

/// Whether injection is enabled: explicit `--exp-ctx-inject` flag, or
/// `CRABCC_EXP_CTX_INJECT` set to a truthy value.
pub fn enabled(flag: bool) -> bool {
    flag || std::env::var("CRABCC_EXP_CTX_INJECT")
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// Resolve the reminder text: the repo's `.crabcc/ctx-inject.md` override
/// if present and non-empty, otherwise the built-in default.
fn context_text(root: &Path) -> String {
    let override_path = root.join(".crabcc").join("ctx-inject.md");
    if let Ok(s) = std::fs::read_to_string(&override_path) {
        let s = s.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    DEFAULT_CONTEXT.to_string()
}

/// Print the SessionStart additionalContext envelope when enabled;
/// otherwise print nothing (the hook becomes a no-op).
pub fn run(root: &Path, flag: bool, _session_id: Option<&str>) -> Result<()> {
    if !enabled(flag) {
        return Ok(());
    }
    let ctx = context_text(root);
    tracing::info!(
        target: "crabcc::shell::context",
        bytes = ctx.len(),
        "injected experimental session context"
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
