//! MCP tool defs + dispatch for `memory.*` tools.
//!
//! Each tool accepts an optional `cwd` arg. Resolution: walk up from `cwd`
//! looking for `.git`, fall back to the path itself, then to the server's
//! startup root if no `cwd` was given. Each call opens its own `Palace`
//! (file-backed via `SqliteBackend`); a registry-cached variant is the
//! M3-full upgrade once we measure perf.
//!
//! Optional `session_id` arg propagates through to drawer rows for
//! per-call grouping (terminal id from CLI, conversation id from MCP).

use crate::{arg_str, str_field, tool_schema as tool};
use anyhow::{anyhow, Result};
use crabcc_memory::{
    find_git_root,
    mine::{project::MineProjectOpts, sessions::MineSessionsOpts},
    DeleteSel, Palace, DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_PAIR_BYTES,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn tools_def() -> Vec<Value> {
    let cwd_field = json!({
        "type": "string",
        "description": "Path inside a git repo. Server walks up to find the repo root \
                        and opens .crabcc/memory.db there. Defaults to server startup cwd."
    });
    let session_field = json!({
        "type": "string",
        "description": "Optional session id (e.g., conversation id or terminal id) \
                        recorded on stored drawers for per-invocation grouping."
    });
    vec![
        tool(
            "memory.init",
            "Idempotent open-or-create for the memory store at <repo>/.crabcc/memory.db.",
            json!({"cwd": cwd_field}),
            &[],
        ),
        tool(
            "memory.remember",
            "Store one drawer (manual entry). For bulk mining wait for M1.",
            json!({
                "cwd":     cwd_field,
                "source":  str_field("source identifier (file path / convo id / free string)"),
                "body":    str_field("verbatim drawer body"),
                "wing":    str_field("wing bucket — defaults to 'default'"),
                "room":    str_field("optional room sub-bucket"),
                "session_id": session_field,
            }),
            &["source", "body"],
        ),
        tool(
            "memory.search",
            "Search top-K drawers. Default mode is hybrid (BM25 + vector via \
             Reciprocal Rank Fusion). Pass `mode: \"lexical\"` for BM25-only \
             or `mode: \"vector\"` for KNN-only ablations.",
            json!({
                "cwd":   cwd_field,
                "query": str_field("query text"),
                "limit": {"type": "integer", "description": "max hits (default 10)"},
                "wing":  str_field("optional wing filter"),
                "room":  str_field("optional room filter"),
                "mode":  str_field("hybrid (default) | lexical | vector"),
            }),
            &["query"],
        ),
        tool(
            "memory.get",
            "Fetch one drawer verbatim by id. Returns null if not found.",
            json!({"cwd": cwd_field, "id": {"type": "integer"}}),
            &["id"],
        ),
        tool(
            "memory.list",
            "List drawers (no similarity). Optional wing filter; ordered by id ASC.",
            json!({
                "cwd":   cwd_field,
                "wing":  str_field("optional wing filter"),
                "limit": {"type": "integer", "description": "max rows (default 50)"},
            }),
            &[],
        ),
        tool(
            "memory.delete",
            "Delete drawers. Specify exactly one of: id, source, all.",
            json!({
                "cwd":    cwd_field,
                "id":     {"type": "integer"},
                "source": str_field("source identifier"),
                "all":    {"type": "boolean"},
            }),
            &[],
        ),
        tool(
            "memory.forget",
            "Delete drawers and run VACUUM to reclaim disk. Specify either \
             {drawer} or {wing, before}. Idempotent on missing IDs / empty \
             windows. `before` accepts epoch seconds or RFC3339.",
            json!({
                "cwd":    cwd_field,
                "drawer": {"type": "integer"},
                "wing":   str_field("wing name"),
                "before": str_field("cutoff: epoch seconds or RFC3339"),
            }),
            &[],
        ),
        tool(
            "memory.backup",
            "Write a transactionally consistent snapshot of memory.db to `dest` \
             using VACUUM INTO. Safe on a live WAL-mode database — no WAL sidecar \
             in the output, readers/writers unblocked. Prefer over raw file copy.",
            json!({
                "cwd":  cwd_field,
                "dest": str_field("destination file path for the snapshot"),
            }),
            &["dest"],
        ),
        tool(
            "memory.count",
            "Drawer count for the store.",
            json!({"cwd": cwd_field}),
            &[],
        ),
        tool(
            "memory.health",
            "Health snapshot: Ok / Degraded / Down.",
            json!({"cwd": cwd_field}),
            &[],
        ),
        tool(
            "memory.mine_project",
            "Walk a repository and store one drawer per text file under \
             wing=\"proj\". Idempotent on re-run via (source_id, sha256). \
             Returns a MineReport: {scanned, considered, inserted, deduped, skipped}.",
            json!({
                "cwd":        cwd_field,
                "path":       str_field("dir to walk; defaults to memory-store root"),
                "max_bytes":  {"type": "integer", "description":
                    "per-file body cap (default 1_000_000)"},
                "session_id": session_field,
            }),
            &[],
        ),
        tool(
            "memory.mine_sessions",
            "Walk a JSONL directory of Claude Code transcripts and store \
             one drawer per (user, assistant) turn pair under wing=\"session\". \
             Idempotent. `dir` defaults to $HOME/.claude/projects/.",
            json!({
                "cwd":             cwd_field,
                "dir":             str_field("dir or single .jsonl file"),
                "max_pair_bytes":  {"type": "integer", "description":
                    "per-pair body cap (default 65536)"},
                "session_id":      session_field,
            }),
            &[],
        ),
        // ── remind (send_later primitive) ────────────────────────────────────
        tool(
            "memory.remind_set",
            "Schedule a reminder. Pass either `due_at` (absolute epoch seconds) or \
             `delay` (human string: '1h30m', '2d', '45m', '90s'). \
             Returns {id, due_at}.",
            json!({
                "cwd":     cwd_field,
                "message": str_field("reminder message surfaced when the reminder fires"),
                "due_at":  {"type": "integer",
                            "description": "absolute epoch seconds (mutually exclusive with delay)"},
                "delay":   str_field("relative duration, e.g. '1h', '30m', '2d' (mutually exclusive with due_at)"),
            }),
            &["message"],
        ),
        tool(
            "memory.remind_poll",
            "Atomically fetch all due reminders (due_at ≤ now, not yet delivered) \
             and mark them delivered. Returns [] when nothing is due. \
             Call this from a PostToolUse hook / agent startup to emulate send_later \
             across Claude Code, OpenCode, Cursor, Nullclaw, OMP, and any MCP client.",
            json!({"cwd": cwd_field}),
            &[],
        ),
        tool(
            "memory.remind_list",
            "List reminders without marking them delivered. \
             Pass include_delivered: true to see fired ones too.",
            json!({
                "cwd": cwd_field,
                "include_delivered": {"type": "boolean",
                                      "description": "include already-delivered reminders (default false)"},
            }),
            &[],
        ),
        tool(
            "memory.remind_delete",
            "Cancel a scheduled reminder by id. Returns {deleted: true|false}.",
            json!({
                "cwd": cwd_field,
                "id":  {"type": "integer"},
            }),
            &["id"],
        ),
        tool(
            "memory.remind_hooks",
            "Print per-agent hook configuration snippets so `memory.remind_poll` \
             fires automatically on every tool use / session start. \
             Covers: claude-code, opencode, cursor, nullclaw, omp, shell (bash/zsh), generic-mcp. \
             Pass `agent` to filter to one agent.",
            json!({
                "agent": str_field("optional: claude-code | opencode | cursor | nullclaw | omp | shell | generic-mcp"),
            }),
            &[],
        ),
    ]
}

/// Dispatch a `memory.*` tool. Returns the same compact-JSON string the CLI
/// would print to stdout for the equivalent subcommand.
pub fn dispatch(tool: &str, args: &Value, server_root: &Path) -> Result<String> {
    let palace = open_palace(args, server_root)?;
    match tool {
        "memory.init" => {
            Ok(json!({"status": "ok", "root": palace.root.display().to_string()}).to_string())
        }
        "memory.remember" => {
            let source = arg_str(args, "source")?;
            let body = arg_str(args, "body")?;
            let wing = args
                .get("wing")
                .and_then(|v| v.as_str())
                .unwrap_or("default");
            let room = args.get("room").and_then(|v| v.as_str());
            let session = args.get("session_id").and_then(|v| v.as_str());
            let id = palace.remember_in_session(wing, room, source, body, session)?;
            Ok(json!({"id": id}).to_string())
        }
        "memory.search" => {
            let q = arg_str(args, "query")?;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let wing = args.get("wing").and_then(|v| v.as_str());
            let room = args.get("room").and_then(|v| v.as_str());
            let mode = args
                .get("mode")
                .and_then(|v| v.as_str())
                .map(|s| {
                    crabcc_memory::SearchMode::parse(s).ok_or_else(|| {
                        anyhow!("invalid mode {s:?}; expected hybrid|lexical|vector")
                    })
                })
                .transpose()?
                .unwrap_or_default();
            let r = palace.search_with_mode(mode, q, limit, wing, room)?;
            Ok(serde_json::to_string(&r)?)
        }
        "memory.get" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("missing arg: id"))?;
            match palace.get(id)? {
                Some(d) => Ok(serde_json::to_string(&d)?),
                None => Ok("null".to_string()),
            }
        }
        "memory.list" => {
            let wing = args.get("wing").and_then(|v| v.as_str());
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            let drawers = palace.list_drawers(wing, limit)?;
            Ok(serde_json::to_string(&drawers)?)
        }
        "memory.delete" => {
            let id = args.get("id").and_then(|v| v.as_i64());
            let source = args
                .get("source")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let all = args
                .get("all")
                .and_then(|v| v.as_bool())
                .unwrap_or_default();
            let count = [id.is_some(), source.is_some(), all]
                .iter()
                .filter(|x| **x)
                .count();
            if count != 1 {
                return Err(anyhow!("specify exactly one of id, source, all"));
            }
            let sel = if all {
                DeleteSel::All
            } else if let Some(i) = id {
                DeleteSel::ById(vec![i])
            } else {
                DeleteSel::BySource(source.unwrap())
            };
            let n = palace.delete(&sel)?;
            Ok(json!({"deleted": n}).to_string())
        }
        "memory.forget" => {
            let drawer = args.get("drawer").and_then(|v| v.as_i64());
            let wing = args.get("wing").and_then(|v| v.as_str());
            let before_raw = args.get("before").and_then(|v| v.as_str());
            let sel = match (drawer, wing, before_raw) {
                (Some(id), None, None) => DeleteSel::ById(vec![id]),
                (None, Some(w), Some(b)) => {
                    let before = b
                        .parse::<i64>()
                        .ok()
                        .or_else(|| parse_rfc3339_to_epoch(b))
                        .ok_or_else(|| {
                            anyhow!("`before` must be epoch seconds or RFC3339, got {b:?}")
                        })?;
                    DeleteSel::BeforeInWing {
                        wing: w.to_string(),
                        before,
                    }
                }
                _ => {
                    return Err(anyhow!(
                        "specify either {{drawer}} or {{wing, before}} (mutually exclusive)"
                    ))
                }
            };
            let n = palace.forget(&sel)?;
            Ok(json!({"forgotten": n}).to_string())
        }
        "memory.backup" => {
            let dest = arg_str(args, "dest")?;
            let dest_path = std::path::Path::new(dest);
            palace.vacuum_into(dest_path)?;
            Ok(json!({"status": "ok", "dest": dest}).to_string())
        }
        "memory.count" => Ok(json!({"count": palace.count()?}).to_string()),
        "memory.health" => Ok(serde_json::to_string(&palace.health())?),
        "memory.mine_project" => {
            let target: PathBuf = args
                .get("path")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| palace.root.clone());
            let max_bytes = args
                .get("max_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_MAX_FILE_BYTES);
            let session = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let opts = MineProjectOpts {
                max_bytes,
                session_id: session,
            };
            let report = palace.mine_project(&target, &opts)?;
            Ok(serde_json::to_string(&report)?)
        }
        "memory.mine_sessions" => {
            let target = args
                .get("dir")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(default_sessions_dir);
            let max_pair_bytes = args
                .get("max_pair_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_MAX_PAIR_BYTES as u64) as usize;
            let session = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let opts = MineSessionsOpts {
                max_pair_bytes,
                session_id: session,
            };
            let report = palace.mine_sessions(&target, &opts)?;
            Ok(serde_json::to_string(&report)?)
        }
        "memory.remind_set" => {
            let message = arg_str(args, "message")?;
            let due_at = if let Some(n) = args.get("due_at").and_then(|v| v.as_i64()) {
                n
            } else if let Some(s) = args.get("delay").and_then(|v| v.as_str()) {
                parse_due_at(s)
                    .ok_or_else(|| anyhow!("invalid delay {s:?}; use '1h30m', '2d', '45m'"))?
            } else {
                return Err(anyhow!("specify either due_at (epoch seconds) or delay ('1h30m')"));
            };
            let id = palace.remind_set(due_at, message)?;
            Ok(json!({"id": id, "due_at": due_at}).to_string())
        }
        "memory.remind_poll" => {
            let due = palace.remind_poll()?;
            Ok(serde_json::to_string(&due)?)
        }
        "memory.remind_list" => {
            let include_delivered = args
                .get("include_delivered")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let reminders = palace.remind_list(include_delivered)?;
            Ok(serde_json::to_string(&reminders)?)
        }
        "memory.remind_delete" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("missing arg: id"))?;
            let deleted = palace.remind_delete(id)?;
            Ok(json!({"deleted": deleted}).to_string())
        }
        "memory.remind_hooks" => {
            let agent = args.get("agent").and_then(|v| v.as_str());
            Ok(serde_json::to_string_pretty(&remind_hooks_json(agent))?)
        }
        other => Err(anyhow!("unknown memory tool: {other}")),
    }
}

fn default_sessions_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".claude").join("projects");
    }
    PathBuf::from(".")
}

/// Best-effort capture for symbol-side tool calls. Off unless
/// `CRABCC_AUTO_MEMORY=1`. Errors swallowed by design.
pub fn auto_capture(server_root: &Path, op: &str, query: &str, count: usize, args: &Value) {
    if !env_auto_capture_enabled() {
        return;
    }
    let session = args.get("session_id").and_then(|v| v.as_str());
    auto_capture_inner(server_root, args, op, query, count, session);
}

/// Pure (no env reads) variant of `auto_capture` — exposed for tests and
/// any caller that wants to drive capture without the env-var gate.
pub fn auto_capture_inner(
    server_root: &Path,
    args: &Value,
    op: &str,
    query: &str,
    count: usize,
    session: Option<&str>,
) {
    let _: Result<()> = (|| {
        let palace = open_palace(args, server_root)?;
        let body = format!("{op} \"{query}\" -> {count} hit(s)");
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

/// Parse a user-supplied delay/timestamp into an absolute epoch value.
/// Accepts: bare integer (epoch seconds), RFC3339 string, or human duration
/// ("1h30m", "2d", "45m", "90s"). Returns None on invalid input.
fn parse_due_at(s: &str) -> Option<i64> {
    if let Ok(n) = s.parse::<i64>() {
        return Some(n);
    }
    if s.contains('T') || (s.len() >= 10 && s.as_bytes()[4] == b'-') {
        return parse_rfc3339_to_epoch(s);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut total: i64 = 0;
    let mut num = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
        } else {
            let n: i64 = num.parse().ok()?;
            num.clear();
            total += match ch {
                'd' => n * 86_400,
                'h' => n * 3_600,
                'm' => n * 60,
                's' => n,
                _ => return None,
            };
        }
    }
    if !num.is_empty() || total == 0 {
        return None;
    }
    Some(now + total)
}

/// Per-agent hook configuration for wiring `memory.remind_poll` as a
/// `send_later`-style primitive. Covers every MCP-capable agent in the
/// crabcc ecosystem; filter by `agent` to print a single snippet.
fn remind_hooks_json(agent: Option<&str>) -> Value {
    let poll_cmd = "r=$(crabcc memory remind poll 2>/dev/null); \
                    [ \"$r\" = '[]' ] || [ -z \"$r\" ] || \
                    printf '%s\\n' \"$r\" | jq -r '.[] | \"\\u23f0 Reminder: \" + .message'";

    let shell_snippet = format!(
        "_crabcc_remind() {{\n\
        \x20 local r; r=$(crabcc memory remind poll 2>/dev/null) || return\n\
        \x20 [ \"$r\" = '[]' ] || [ -z \"$r\" ] && return\n\
        \x20 printf '%s\\n' \"$r\" | jq -r '.[] | \"\\u23f0 \" + .message' 2>/dev/null || true\n\
        }}\n\
        # bash — append to PROMPT_COMMAND:\n\
        PROMPT_COMMAND=\"${{PROMPT_COMMAND:+$PROMPT_COMMAND; }}_crabcc_remind\"\n\
        # zsh — add to precmd:\n\
        autoload -Uz add-zsh-hook && add-zsh-hook precmd _crabcc_remind"
    );

    let generic_mcp = "Wire crabcc as an MCP server (command: \"crabcc\", args: [\"--mcp\"]) \
                        and instruct your agent to call memory.remind_poll at session start. \
                        The tool returns [] when nothing is due, or a list of \
                        {id, due_at, message, created_at, delivered: false} objects.";

    let all = json!({
        "claude-code": {
            "description": "PostToolUse hook — fires on every tool call, surfaces due reminders",
            "merge_into": "~/.claude/settings.json (or project .claude/settings.json)",
            "config": {
                "hooks": {
                    "PostToolUse": [{
                        "matcher": "",
                        "hooks": [{"type": "command", "command": poll_cmd}]
                    }]
                }
            }
        },
        "opencode": {
            "description": "System-prompt instruction for OpenCode MCP integration",
            "merge_into": "opencode.json → instructions field, or .opencode/AGENTS.md",
            "snippet": "Call the memory.remind_poll MCP tool at the start of each session \
                        and display any returned reminders before proceeding."
        },
        "cursor": {
            "description": "Agent rule — instructs Cursor to poll on session start",
            "merge_into": ".cursor/rules.md or ~/.cursor/rules.md",
            "snippet": "At the start of each conversation call the memory.remind_poll MCP tool \
                        and surface any returned reminders to the user."
        },
        "nullclaw": {
            "description": "Session-start hook for Nullclaw agent sessions",
            "merge_into": ".nullclaw/rules.md or agent config hooks",
            "snippet": "Invoke memory.remind_poll via the crabcc MCP server at session start \
                        and print any due reminders before the first response."
        },
        "omp": {
            "description": "Hook for OMP / Oh My Posh prompt integration",
            "merge_into": "OMP config (custom segment) or agent startup script",
            "snippet": shell_snippet.clone(),
            "note": "If using OMP as a shell prompt, add the shell snippet instead. \
                     If OMP is an AI agent name, use the generic-mcp config."
        },
        "shell": {
            "description": "PROMPT_COMMAND / precmd — fires on every new terminal prompt",
            "merge_into": "~/.bashrc or ~/.zshrc",
            "snippet": shell_snippet
        },
        "generic-mcp": {
            "description": "For any MCP-compatible agent: aider, continue.dev, custom agents, etc.",
            "snippet": generic_mcp
        }
    });

    match agent {
        Some(name) => all.get(name).cloned().unwrap_or_else(|| {
            json!({
                "error": format!("unknown agent {name:?}"),
                "valid": ["claude-code","opencode","cursor","nullclaw","omp","shell","generic-mcp"]
            })
        }),
        None => all,
    }
}

fn open_palace(args: &Value, server_root: &Path) -> Result<Palace> {
    let cwd = args
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| server_root.to_path_buf());
    let resolved = find_git_root(&cwd).unwrap_or(cwd);
    Palace::open(&resolved)
}

/// RFC3339 → epoch-seconds. Tiny parser, no chrono dep — handles
/// `YYYY-MM-DDTHH:MM:SSZ` shape (the only one the CLI advertises).
/// Returns None on anything weirder; callers turn that into an error.
fn parse_rfc3339_to_epoch(s: &str) -> Option<i64> {
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
    // Howard Hinnant's days-from-epoch algorithm.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- parse_rfc3339_to_epoch ---

    #[test]
    fn rfc3339_unix_epoch() {
        // 1970-01-01T00:00:00Z == 0
        assert_eq!(parse_rfc3339_to_epoch("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn rfc3339_known_timestamp() {
        // 2024-01-15T12:30:45Z — verified via the algorithm
        let result = parse_rfc3339_to_epoch("2024-01-15T12:30:45Z");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), 1705321845);
    }

    #[test]
    fn rfc3339_rejects_short_input() {
        assert_eq!(parse_rfc3339_to_epoch("2024-01-15"), None);
        assert_eq!(parse_rfc3339_to_epoch(""), None);
        assert_eq!(parse_rfc3339_to_epoch("short"), None);
    }

    #[test]
    fn rfc3339_rejects_bad_format() {
        // Wrong separators
        assert_eq!(parse_rfc3339_to_epoch("2024/01/15T12:30:45Z"), None);
        assert_eq!(parse_rfc3339_to_epoch("2024-01-15 12:30:45Z"), None); // space not T
    }

    #[test]
    fn rfc3339_rejects_non_numeric_fields() {
        assert_eq!(parse_rfc3339_to_epoch("XXXX-01-15T12:30:45Z"), None);
        assert_eq!(parse_rfc3339_to_epoch("2024-XX-15T12:30:45Z"), None);
    }

    // --- arg_str ---

    #[test]
    fn arg_str_extracts_string() {
        let v = json!({"name": "hello", "count": 42});
        assert_eq!(arg_str(&v, "name").unwrap(), "hello");
    }

    #[test]
    fn arg_str_missing_key_errors() {
        let v = json!({"name": "hello"});
        let err = arg_str(&v, "missing").unwrap_err();
        assert!(err.to_string().contains("missing arg: missing"));
    }

    #[test]
    fn arg_str_non_string_value_errors() {
        let v = json!({"count": 42});
        let err = arg_str(&v, "count").unwrap_err();
        assert!(err.to_string().contains("missing arg: count"));
    }

    // --- tools_def ---

    #[test]
    fn tools_def_returns_all_memory_tools() {
        let tools = tools_def();
        // At least 10 tools; may grow as new tools land
        assert!(
            tools.len() >= 10,
            "expected >=10 tools, got {}",
            tools.len()
        );
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"memory.init"));
        assert!(names.contains(&"memory.remember"));
        assert!(names.contains(&"memory.search"));
        assert!(names.contains(&"memory.get"));
        assert!(names.contains(&"memory.list"));
        assert!(names.contains(&"memory.delete"));
        assert!(names.contains(&"memory.forget"));
        assert!(names.contains(&"memory.count"));
        assert!(names.contains(&"memory.health"));
        assert!(names.contains(&"memory.mine_project"));
    }

    #[test]
    fn tools_def_have_input_schema() {
        for tool in tools_def() {
            assert!(
                tool.get("inputSchema").is_some(),
                "tool {} missing inputSchema",
                tool["name"]
            );
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    // --- dispatch ---

    /// Pin `$CRABCC_HOME` to a single tempdir for the entire test
    /// process so `Palace::open(repo_root)` (which now routes through
    /// `$CRABCC_HOME/repos/<slug>/memory.db` since #479) doesn't
    /// pollute the user's real `~/.crabcc/`. The leaked TempDir
    /// guard keeps the directory alive for the test process.
    use crate::test_support::ensure_test_crabcc_home;

    #[test]
    fn dispatch_unknown_tool_errors() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        let err = dispatch("memory.nonexistent", &json!({}), dir.path()).unwrap_err();
        assert!(err.to_string().contains("unknown memory tool"));
    }

    #[test]
    fn dispatch_init_creates_db() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        // Create a .git dir so find_git_root resolves
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let result = dispatch(
            "memory.init",
            &json!({"cwd": dir.path().to_str()}),
            dir.path(),
        );
        assert!(result.is_ok(), "init failed: {:?}", result.err());
        let parsed: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(parsed["status"], "ok");
    }

    #[test]
    fn dispatch_count_on_fresh_db() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let result = dispatch(
            "memory.count",
            &json!({"cwd": dir.path().to_str()}),
            dir.path(),
        );
        assert!(result.is_ok());
        let parsed: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(parsed["count"], 0);
    }

    #[test]
    fn dispatch_remember_and_get() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let cwd = dir.path().to_str().unwrap();

        // Remember a drawer
        let args = json!({
            "cwd": cwd,
            "source": "test-src",
            "body": "hello world",
        });
        let result = dispatch("memory.remember", &args, dir.path()).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        let id = parsed["id"].as_i64().unwrap();
        assert!(id > 0);

        // Get it back
        let args = json!({"cwd": cwd, "id": id});
        let result = dispatch("memory.get", &args, dir.path()).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["body"], "hello world");
        assert_eq!(parsed["source_id"], "test-src");
    }

    #[test]
    fn dispatch_delete_requires_exactly_one_selector() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let cwd = dir.path().to_str().unwrap();

        // No selector
        let err = dispatch("memory.delete", &json!({"cwd": cwd}), dir.path()).unwrap_err();
        assert!(err.to_string().contains("specify exactly one"));

        // Multiple selectors
        let err = dispatch(
            "memory.delete",
            &json!({"cwd": cwd, "id": 1, "all": true}),
            dir.path(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("specify exactly one"));
    }

    #[test]
    fn dispatch_forget_rejects_invalid_args() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let cwd = dir.path().to_str().unwrap();

        // Neither {drawer} nor {wing, before}
        let err = dispatch("memory.forget", &json!({"cwd": cwd}), dir.path()).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn dispatch_list_on_fresh_db_returns_empty() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let cwd = dir.path().to_str().unwrap();

        let result = dispatch("memory.list", &json!({"cwd": cwd}), dir.path()).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 0);
    }

    #[test]
    fn dispatch_health_on_fresh_db() {
        ensure_test_crabcc_home();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let cwd = dir.path().to_str().unwrap();

        let result = dispatch("memory.health", &json!({"cwd": cwd}), dir.path()).unwrap();
        // Health returns the HealthStatus enum serialized as a string
        let parsed: Value = serde_json::from_str(&result).unwrap();
        // Could be "Ok" string or {"status":"Ok"} — check either form
        let is_ok =
            parsed == json!("Ok") || parsed.get("status").and_then(|v| v.as_str()) == Some("Ok");
        assert!(is_ok, "expected Ok health, got: {parsed}");
    }

    // --- env_auto_capture_enabled ---

    #[test]
    fn auto_capture_disabled_by_default() {
        // In test env, CRABCC_AUTO_MEMORY is typically not set
        std::env::remove_var("CRABCC_AUTO_MEMORY");
        assert!(!env_auto_capture_enabled());
    }
}
