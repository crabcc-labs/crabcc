//! Session miner — drawer per `(user, assistant)` turn pair.
//!
//! Targets Claude Code's per-conversation transcript files, typically
//! at `~/.claude/projects/<repo-slug>/<uuid>.jsonl`. Each line is a
//! self-contained event; we collapse adjacent `user` → `assistant`
//! pairs into one drawer so the body matches the granularity at which
//! a user might ask "what did I tell you about X?".
//!
//! The line format is permissive: any object with a `message.role` and
//! `message.content` is acceptable. `content` may be either a plain
//! string (legacy) or an array of typed parts (current); we extract
//! `text` parts and concatenate. Tool-call / tool-result parts are
//! dropped because they bloat embeddings and rarely improve recall.

use super::{MineReport, SkipReason};
use crate::Palace;
use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Per-pair body cap. Blowing past this is a warning sign that the user
/// pasted a wall of text; we'd rather lose the pair than blow up FTS5.
pub const DEFAULT_MAX_PAIR_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct MineSessionsOpts {
    pub max_pair_bytes: usize,
    pub session_id: Option<String>,
}

impl Default for MineSessionsOpts {
    fn default() -> Self {
        Self {
            max_pair_bytes: DEFAULT_MAX_PAIR_BYTES,
            session_id: None,
        }
    }
}

/// Walk `dir` for `*.jsonl` files and emit a drawer per turn pair.
/// `dir` may be a single JSONL file or a directory; both shapes work.
pub fn mine_sessions(palace: &Palace, dir: &Path, opts: &MineSessionsOpts) -> Result<MineReport> {
    let mut report = MineReport::default();
    let files = collect_jsonl_files(dir)?;
    for file in files {
        process_file(palace, &file, opts, &mut report)?;
    }
    Ok(report)
}

fn collect_jsonl_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if dir.is_file() {
        return Ok(vec![dir.to_path_buf()]);
    }
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in crabcc_core::walker::walk_repo(dir) {
        if entry.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            out.push(entry);
        }
    }
    out.sort_unstable();
    Ok(out)
}

fn process_file(
    palace: &Palace,
    path: &Path,
    opts: &MineSessionsOpts,
    report: &mut MineReport,
) -> Result<()> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) => {
            tracing::debug!(target: "crabcc_memory::mine", "session read fail {}: {err}", path.display());
            return Ok(());
        }
    };

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session");

    let mut pending_user: Option<String> = None;
    let mut pair_index: usize = 0;
    let mut compact_index: usize = 0;

    for line in raw.lines() {
        report.scanned += 1;
        let line = line.trim();
        if line.is_empty() {
            report.record_skip();
            continue;
        }
        let evt = match serde_json::from_str::<RawEvent>(line) {
            Ok(e) => e,
            Err(_) => {
                log_skip(SkipReason::Unparseable);
                report.record_skip();
                continue;
            }
        };

        if evt.r#type.as_deref() == Some("compact") {
            if std::env::var("CRABCC_COMPACT_HOOK").as_deref() == Ok("1") {
                compact_index += 1;
                let body = format!(
                    "compact session={} code_len={}",
                    evt.session_id.as_deref().unwrap_or("unknown"),
                    evt.original_code.as_deref().map(|s| s.len()).unwrap_or(0),
                );
                // Use compact_index (stable per-file counter) instead of report.scanned
                // so that inserting/removing unrelated lines before this entry does not
                // change the source_id and cause spurious re-insertion on re-runs.
                let source_id = format!("compact:{}:{}", stem, compact_index);
                let pre = palace.count()?;
                palace.remember("compact", Some("raw"), &source_id, &body)?;
                let post = palace.count()?;
                if post > pre {
                    report.record_inserted();
                } else {
                    report.record_dedup();
                }
            } else {
                report.record_skip();
            }
            continue;
        }

        let role = evt
            .message
            .as_ref()
            .map(|m| m.role.as_str())
            .unwrap_or_default();
        let text = evt.message.as_ref().map(|m| m.text()).unwrap_or_default();
        if text.is_empty() {
            report.record_skip();
            continue;
        }

        match role {
            "user" => {
                pending_user = Some(text);
            }
            "assistant" => {
                let user = pending_user.take().unwrap_or_default();
                pair_index += 1;
                let body = format_pair(&user, &text, opts.max_pair_bytes);
                if body.is_empty() {
                    report.record_skip();
                    continue;
                }
                let source_id = format!("session:{stem}:{pair_index}");
                let pre = palace.count()?;
                palace.remember_in_session(
                    "session",
                    Some("turn"),
                    &source_id,
                    &body,
                    opts.session_id.as_deref(),
                )?;
                let post = palace.count()?;
                if post > pre {
                    report.record_inserted();
                } else {
                    report.record_dedup();
                }
            }
            _ => {
                // system / tool / unknown — not part of the user-facing
                // dialogue, skip.
                report.record_skip();
            }
        }
    }

    Ok(())
}

fn format_pair(user: &str, assistant: &str, cap: usize) -> String {
    let user = user.trim();
    let assistant = assistant.trim();
    if assistant.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(user.len() + assistant.len() + 16);
    if !user.is_empty() {
        out.push_str("USER: ");
        out.push_str(user);
        out.push_str("\n\n");
    }
    out.push_str("ASSISTANT: ");
    out.push_str(assistant);
    if out.len() > cap {
        out.truncate(cap);
    }
    out
}

#[derive(Debug, Deserialize)]
struct RawEvent {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    message: Option<RawMessage>,
    // compact entry fields
    #[serde(default)]
    original_code: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: serde_json::Value,
}

impl RawMessage {
    /// Concatenate every `text` substring out of `content`. Accepts:
    /// - bare string (legacy Claude Code transcripts)
    /// - array of `{type:"text", text:"..."}` parts (current)
    /// - array containing tool-use/tool-result objects (drop those)
    fn text(&self) -> String {
        match &self.content {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(parts) => {
                let mut buf = String::new();
                for p in parts {
                    if let Some(t) = p.get("type").and_then(|v| v.as_str()) {
                        if t == "text" {
                            if let Some(s) = p.get("text").and_then(|v| v.as_str()) {
                                if !buf.is_empty() {
                                    buf.push('\n');
                                }
                                buf.push_str(s);
                            }
                        }
                    }
                }
                buf
            }
            _ => String::new(),
        }
    }
}

fn log_skip(reason: SkipReason) {
    tracing::debug!(target: "crabcc_memory::mine", "session skip: {reason:?}");
}

/// Test/bench helper — write a single JSONL file from a list of
/// `(role, text)` pairs in the order they should appear.
pub fn write_synthetic_jsonl<P: AsRef<Path>>(path: P, turns: &[(&str, &str)]) -> Result<PathBuf> {
    let path = path.as_ref().to_path_buf();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut buf = String::new();
    for (role, text) in turns {
        let evt = serde_json::json!({
            "message": {"role": role, "content": text}
        });
        buf.push_str(&evt.to_string());
        buf.push('\n');
    }
    std::fs::write(&path, buf)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;

    // Serialize tests that mutate CRABCC_COMPACT_HOOK to avoid races
    // (env is process-global; cargo test runs threads in parallel by default).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn pairs_user_assistant_into_drawers() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("conv.jsonl");
        write_synthetic_jsonl(
            &f,
            &[
                ("user", "what did I tell you about Apricots?"),
                ("assistant", "you mentioned they ripen in summer."),
                ("user", "and pears?"),
                ("assistant", "you said they bruise easily."),
            ],
        )
        .unwrap();
        let palace = Palace::ephemeral();

        let report = mine_sessions(&palace, dir.path(), &MineSessionsOpts::default()).unwrap();

        assert_eq!(report.inserted, 2, "two complete pairs => two drawers");
        let hits = palace.search("Apricots", 5).unwrap().hits;
        assert!(hits.iter().any(|h| h.body.contains("Apricots")));
    }

    #[test]
    fn rerun_is_idempotent() {
        let dir = tempdir().unwrap();
        write_synthetic_jsonl(
            dir.path().join("c.jsonl"),
            &[("user", "ping"), ("assistant", "pong")],
        )
        .unwrap();
        let palace = Palace::ephemeral();
        mine_sessions(&palace, dir.path(), &MineSessionsOpts::default()).unwrap();
        let again = mine_sessions(&palace, dir.path(), &MineSessionsOpts::default()).unwrap();
        assert_eq!(again.inserted, 0);
        assert_eq!(again.deduped, 1);
        assert_eq!(palace.count().unwrap(), 1);
    }

    #[test]
    fn handles_typed_content_arrays() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("typed.jsonl");
        let lines = [
            serde_json::json!({"message": {"role": "user", "content": [
                {"type": "text", "text": "what about figs?"}
            ]}}),
            serde_json::json!({"message": {"role": "assistant", "content": [
                {"type": "text", "text": "figs love sunshine."},
                {"type": "tool_use", "name": "search", "input": {"q": "figs"}},
            ]}}),
        ];
        let buf: String = lines.iter().map(|j| format!("{j}\n")).collect();
        std::fs::write(&f, buf).unwrap();
        let palace = Palace::ephemeral();

        let report = mine_sessions(&palace, dir.path(), &MineSessionsOpts::default()).unwrap();

        assert_eq!(report.inserted, 1);
        let only = palace.list_drawers(Some("session"), 5).unwrap();
        assert!(only[0].body.contains("figs love sunshine"));
        assert!(
            !only[0].body.contains("tool_use"),
            "tool-use parts must be stripped"
        );
    }

    #[test]
    fn unparseable_lines_skip_not_error() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("messy.jsonl");
        let body = concat!(
            "{not json}\n",
            r#"{"message": {"role": "user", "content": "ok"}}"#,
            "\n",
            r#"{"message": {"role": "assistant", "content": "fine"}}"#,
            "\n",
            "\n",
        );
        std::fs::write(&f, body).unwrap();
        let palace = Palace::ephemeral();

        let report = mine_sessions(&palace, dir.path(), &MineSessionsOpts::default()).unwrap();

        assert_eq!(report.inserted, 1);
        assert!(report.skipped >= 1);
    }

    #[test]
    fn dangling_user_without_assistant_does_not_emit() {
        let dir = tempdir().unwrap();
        write_synthetic_jsonl(
            dir.path().join("hanging.jsonl"),
            &[
                ("user", "first prompt"),
                ("user", "second prompt"),
                ("assistant", "answer to second only"),
            ],
        )
        .unwrap();
        let palace = Palace::ephemeral();

        let report = mine_sessions(&palace, dir.path(), &MineSessionsOpts::default()).unwrap();

        // Two `user` frames in a row → second overwrites first; only one
        // pair lands. The earlier user content is dropped (matches what
        // a reader would expect: the assistant only saw the second user
        // prompt).
        assert_eq!(report.inserted, 1);
        let only = palace.list_drawers(Some("session"), 5).unwrap();
        assert!(only[0].body.contains("second prompt"));
    }

    #[test]
    fn directory_with_multiple_files_is_walked() {
        let dir = tempdir().unwrap();
        write_synthetic_jsonl(
            dir.path().join("a.jsonl"),
            &[("user", "alpha-q"), ("assistant", "alpha-a")],
        )
        .unwrap();
        write_synthetic_jsonl(
            dir.path().join("b.jsonl"),
            &[("user", "beta-q"), ("assistant", "beta-a")],
        )
        .unwrap();
        let palace = Palace::ephemeral();

        let report = mine_sessions(&palace, dir.path(), &MineSessionsOpts::default()).unwrap();

        assert_eq!(report.inserted, 2);
        let drawers = palace.list_drawers(Some("session"), 10).unwrap();
        assert!(drawers
            .iter()
            .any(|d| d.source_id.starts_with("session:a:")));
        assert!(drawers
            .iter()
            .any(|d| d.source_id.starts_with("session:b:")));
    }

    #[test]
    fn compact_jsonl_entry_stored_when_hook_enabled() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let f = dir.path().join("compact.jsonl");
        let line = serde_json::json!({
            "type": "compact",
            "session_id": "s1",
            "original_code": "fn main(){}"
        });
        std::fs::write(&f, format!("{line}\n")).unwrap();
        let palace = Palace::ephemeral();

        unsafe { std::env::set_var("CRABCC_COMPACT_HOOK", "1") };
        let report =
            mine_sessions(&palace, dir.path(), &MineSessionsOpts::default()).unwrap();
        unsafe { std::env::remove_var("CRABCC_COMPACT_HOOK") };

        assert_eq!(report.inserted, 1, "compact entry should be stored");
        let drawers = palace.list_drawers(Some("compact"), 5).unwrap();
        assert_eq!(drawers.len(), 1);
        assert!(drawers[0].body.contains("session=s1"));
        assert!(drawers[0].body.contains("code_len=11"));
    }
}
