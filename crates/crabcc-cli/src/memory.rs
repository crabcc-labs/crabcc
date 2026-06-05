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
use serde_json::json;
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
    /// Schedule, poll, list, or cancel reminders — the crabcc `send_later`
    /// primitive. Works across Claude Code, OpenCode, Cursor, Nullclaw, OMP,
    /// and any agent that can call MCP tools or run shell hooks.
    Remind {
        #[command(subcommand)]
        action: RemindCmd,
    },
    /// Summary of compact pipeline history: token savings, avg smoothness.
    /// Gated by CRABCC_COMPACT_HOOK=1; returns zeroes when unset.
    CompactStats,
    /// Ingest URLs and/or freeform text into memory. Mirrors the HTTP
    /// `POST /api/memory/ingest` surface so the CLI and dashboard agree
    /// on drawer ids (`web:<hash>` for URLs, `text:<hash>` for text).
    /// Pass `-` to read text from stdin.
    Ingest {
        /// URL to fetch + clean + store. Repeatable.
        #[arg(long = "url", value_name = "URL")]
        urls: Vec<String>,
        /// Freeform text to store as one drawer. URLs found in the
        /// text are also fetched + stored as their own drawers.
        #[arg(long)]
        text: Option<String>,
        /// Read text from stdin (`-` form). Equivalent to
        /// `--text "$(cat)"` but keeps shell quoting simple.
        #[arg(long)]
        stdin: bool,
        /// Source label baked into each drawer's wing. Defaults to
        /// `cli-ingest`; the HTTP path uses `web-ingest`.
        #[arg(long, default_value = "cli-ingest")]
        source: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum RemindCmd {
    /// Schedule a reminder. Use --in for a relative delay or --at for an
    /// absolute epoch / RFC3339 timestamp.
    ///
    /// Examples:
    ///   crabcc memory remind set --in 1h "check PR #722"
    ///   crabcc memory remind set --in 30m "run benchmarks"
    ///   crabcc memory remind set --at 2026-06-06T09:00:00Z "standup"
    Set {
        /// Reminder message surfaced when the reminder fires.
        message: String,
        /// Relative delay: '1h', '30m', '2d', '1h30m', '90s'.
        #[arg(long = "in", value_name = "DELAY", conflicts_with = "at")]
        delay: Option<String>,
        /// Absolute timestamp: epoch seconds or RFC3339 (e.g. 2026-01-15T12:00:00Z).
        #[arg(long, value_name = "TIMESTAMP", conflicts_with = "delay")]
        at: Option<String>,
    },
    /// Atomically fetch all due reminders and mark them delivered.
    /// Returns [] when nothing is due. Wire as a PostToolUse hook or
    /// PROMPT_COMMAND to get send_later behaviour in any agent.
    Poll,
    /// List scheduled reminders without marking them delivered.
    List {
        /// Include already-delivered reminders.
        #[arg(long)]
        all: bool,
    },
    /// Cancel a scheduled reminder by id.
    Delete { id: i64 },
    /// Print per-agent hook config snippets for wiring remind_poll automatically.
    /// Covers: claude-code, opencode, cursor, nullclaw, omp, shell, generic-mcp.
    Hooks {
        /// Filter output to a single agent.
        #[arg(long, value_name = "AGENT")]
        agent: Option<String>,
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

/// Tiny RFC3339 parser (no chrono dep). Handles `YYYY-MM-DDTHH:MM:SSZ`,
/// `YYYY-MM-DDTHH:MM:SS+00:00`, and any `±HH:MM` offset.
/// Returns None on anything malformed; the caller turns that into an error.
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
    // Parse timezone offset so e.g. `09:00:00-04:00` → 13:00 UTC, not 09:00 UTC.
    let offset_secs: i64 = match bytes.get(19)? {
        b'Z' => 0,
        sign @ (b'+' | b'-') => {
            if bytes.len() < 25 || bytes[22] != b':' {
                return None;
            }
            let oh: i64 = s.get(20..22)?.parse().ok()?;
            let om: i64 = s.get(23..25)?.parse().ok()?;
            let mag = oh * 3_600 + om * 60;
            if *sign == b'+' { mag } else { -mag }
        }
        _ => return None,
    };
    // Days-from-epoch via the proleptic Gregorian "Howard Hinnant" algorithm.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset_secs)
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
        MemoryCmd::Ingest {
            urls,
            text,
            stdin,
            source,
        } => {
            // Collect freeform text — explicit `--text`, then stdin if requested.
            let mut text_buf = text.unwrap_or_default();
            if stdin {
                use std::io::Read;
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                if !text_buf.is_empty() && !s.is_empty() {
                    text_buf.push('\n');
                }
                text_buf.push_str(&s);
            }

            // De-dup URL set: explicit + linkified-from-text.
            let mut url_set: std::collections::BTreeSet<String> = urls.into_iter().collect();
            if !text_buf.is_empty() {
                for u in crabcc_fetch::extract_urls(&text_buf) {
                    url_set.insert(u);
                }
            }
            let urls: Vec<String> = url_set.into_iter().collect();

            let mut ingested: Vec<serde_json::Value> = Vec::new();
            let mut errors: Vec<serde_json::Value> = Vec::new();
            let session = std::env::var("TERM_SESSION_ID").ok();

            // URL fetch phase — async via a per-call runtime. Single-user
            // CLI so the runtime cost is negligible.
            if !urls.is_empty() {
                // `crabcc memory` runs as the user, so URL ingest is
                // relaxed by default (fetch your own LAN/localhost too);
                // set CRABCC_FETCH_SSRF=on to restore the guard. When
                // enforced, pre-filter unsafe URLs with a clear per-URL
                // error rather than letting them fail mid-fetch.
                let enforce_ssrf = crabcc_fetch::ssrf_enforced(false);
                let safe: Vec<String> = if enforce_ssrf {
                    urls.iter()
                        .filter(|u| match crabcc_fetch::is_ingest_safe_url(u) {
                            Ok(()) => true,
                            Err(reason) => {
                                errors.push(serde_json::json!({"url": u, "error": reason}));
                                false
                            }
                        })
                        .cloned()
                        .collect()
                } else {
                    urls.clone()
                };
                if !safe.is_empty() {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    let mut fetch_opts = crabcc_fetch::FetchOpts::ingest();
                    fetch_opts.enforce_ssrf = enforce_ssrf;
                    let results = rt.block_on(crabcc_fetch::fetch_and_clean(&safe, fetch_opts));
                    for r in results {
                        if r.error.is_some() || r.content_markdown.is_none() {
                            errors.push(serde_json::json!({
                                "url": r.url,
                                "error": r.error.unwrap_or_else(|| "no content extracted".into()),
                            }));
                            continue;
                        }
                        let body = r.content_markdown.unwrap_or_default();
                        let id = format!("web:{}", short_hash(r.url.as_bytes()));
                        match palace.remember_in_session(
                            &source,
                            None,
                            &id,
                            &body,
                            session.as_deref(),
                        ) {
                            Ok(drawer_id) => ingested.push(serde_json::json!({
                                "id": id,
                                "url": r.url,
                                "title": r.title,
                                "kind": "web",
                                "bytes": body.len(),
                                "drawer_id": drawer_id,
                            })),
                            Err(e) => {
                                errors.push(serde_json::json!({"url": id, "error": format!("{e}")}))
                            }
                        }
                    }
                }
            }

            // Standalone-text path — only if there's content beyond the URLs.
            if !text_buf.trim().is_empty() {
                let stripped = strip_urls(&text_buf);
                if !stripped.trim().is_empty() {
                    let id = format!("text:{}", short_hash(text_buf.as_bytes()));
                    let label = format!("{source}:text");
                    match palace.remember_in_session(
                        &label,
                        None,
                        &id,
                        &text_buf,
                        session.as_deref(),
                    ) {
                        Ok(drawer_id) => ingested.push(serde_json::json!({
                            "id": id,
                            "kind": "text",
                            "bytes": text_buf.len(),
                            "drawer_id": drawer_id,
                        })),
                        Err(e) => {
                            errors.push(serde_json::json!({"url": id, "error": format!("{e}")}))
                        }
                    }
                }
            }

            let stats = serde_json::json!({"ok": ingested.len(), "failed": errors.len()});
            println!(
                "{}",
                serde_json::json!({"ingested": ingested, "errors": errors, "stats": stats})
            );
        }
        MemoryCmd::Remind { action } => {
            let palace = Palace::open(root)?;
            match action {
                RemindCmd::Set { message, delay, at } => {
                    let due_at = if let Some(d) = delay {
                        parse_remind_delay(&d)?   // relative: bare int = now + N
                    } else if let Some(a) = at {
                        parse_remind_at(&a)?      // absolute: bare int = epoch
                    } else {
                        anyhow::bail!("specify either --in <delay> or --at <timestamp>");
                    };
                    let id = palace.remind_set(due_at, &message)?;
                    println!("{}", json!({"id": id, "due_at": due_at}));
                }
                RemindCmd::Poll => {
                    let due = palace.remind_poll()?;
                    println!("{}", sonic_rs::to_string(&due)?);
                }
                RemindCmd::List { all } => {
                    let reminders = palace.remind_list(all)?;
                    println!("{}", sonic_rs::to_string(&reminders)?);
                }
                RemindCmd::Delete { id } => {
                    let deleted = palace.remind_delete(id)?;
                    println!("{}", json!({"deleted": deleted}));
                }
                RemindCmd::Hooks { agent } => {
                    let out = remind_hooks_json(agent.as_deref());
                    println!("{}", serde_json::to_string_pretty(&out)?);
                }
            }
        }
        MemoryCmd::CompactStats => {
            let stats = crabcc_compact::compact_memory::compact_stats(&palace)?;
            println!("{}", sonic_rs::to_string(&serde_json::json!({
                "total_compact_sessions": stats.total,
                "total_tokens_saved": stats.total_tokens_saved,
                "avg_readability": stats.avg_readability,
            }))?);
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

/// Parse a relative delay (`--in`) into an absolute epoch.
/// Bare integers are seconds from now ("3600" → now + 1 h), not absolute epochs.
/// Human durations ("1h30m", "2d", "45m", "90s") are also relative.
/// RFC3339 strings are rejected — use `--at` for absolute timestamps.
fn parse_remind_delay(s: &str) -> Result<i64> {
    if s.contains('T') {
        anyhow::bail!(
            "--in expects a relative delay ('1h30m', '45m', '90s', or bare seconds); \
             use --at for absolute timestamps"
        );
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Bare integer → relative seconds from now.
    if let Ok(n) = s.parse::<i64>() {
        return Ok(now + n);
    }
    let mut total: i64 = 0;
    let mut num = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
        } else {
            let n: i64 = num
                .parse()
                .map_err(|_| anyhow!("invalid delay {s:?}"))?;
            num.clear();
            total += match ch {
                'd' => n * 86_400,
                'h' => n * 3_600,
                'm' => n * 60,
                's' => n,
                _ => anyhow::bail!("unknown unit {ch:?} in {s:?}; use d/h/m/s"),
            };
        }
    }
    if !num.is_empty() || total == 0 {
        anyhow::bail!("invalid delay {s:?}; use '1h30m', '2d', '45m', '90s', or bare seconds");
    }
    Ok(now + total)
}

/// Parse an absolute timestamp (`--at`) into an epoch.
/// Accepts epoch seconds (bare integer) or RFC3339 string.
fn parse_remind_at(s: &str) -> Result<i64> {
    if let Ok(n) = s.parse::<i64>() {
        return Ok(n);
    }
    time_parse_rfc3339(s)
        .ok_or_else(|| anyhow!("--at expects epoch seconds or RFC3339, got {s:?}"))
}

/// Per-agent hook config for wiring `memory.remind_poll` as a `send_later`
/// primitive. Identical logic to the MCP module's version; duplicated here
/// to avoid a shared-utils crate for a single function.
fn remind_hooks_json(agent: Option<&str>) -> serde_json::Value {
    let poll_cmd = "r=$(crabcc memory remind poll 2>/dev/null); \
                    [ \"$r\" = '[]' ] || [ -z \"$r\" ] || \
                    printf '%s\\n' \"$r\" | jq -r '.[] | \"\\u23f0 Reminder: \" + .message'";
    let shell_snippet = "_crabcc_remind() {\n\
        \x20 local r; r=$(crabcc memory remind poll 2>/dev/null) || return\n\
        \x20 [ \"$r\" = '[]' ] || [ -z \"$r\" ] && return\n\
        \x20 printf '%s\\n' \"$r\" | jq -r '.[] | \"\\u23f0 \" + .message' 2>/dev/null || true\n\
        }\n\
        # bash:\n\
        PROMPT_COMMAND=\"${PROMPT_COMMAND:+$PROMPT_COMMAND; }_crabcc_remind\"\n\
        # zsh:\n\
        autoload -Uz add-zsh-hook && add-zsh-hook precmd _crabcc_remind";
    let generic = "Wire crabcc as an MCP server (command: \"crabcc\", args: [\"--mcp\"]) \
                   and call memory.remind_poll at session start.";
    let all = json!({
        "claude-code": {
            "description": "PostToolUse hook — fires on every tool call",
            "merge_into": "~/.claude/settings.json",
            "config": {"hooks": {"PostToolUse": [{"matcher": "", "hooks": [{"type": "command", "command": poll_cmd}]}]}}
        },
        "opencode": {
            "description": "System-prompt instruction for OpenCode",
            "snippet": "Call memory.remind_poll at the start of each session and display any returned reminders."
        },
        "cursor": {
            "description": "Agent rule for Cursor",
            "merge_into": ".cursor/rules.md",
            "snippet": "At the start of each conversation call memory.remind_poll and surface any returned reminders."
        },
        "nullclaw": {
            "description": "Session-start hook for Nullclaw",
            "snippet": "Invoke memory.remind_poll via the crabcc MCP server at session start."
        },
        "omp": {
            "description": "OMP hook or shell prompt integration",
            "snippet": shell_snippet
        },
        "shell": {
            "description": "PROMPT_COMMAND / precmd — fires on every new prompt",
            "merge_into": "~/.bashrc or ~/.zshrc",
            "snippet": shell_snippet
        },
        "generic-mcp": {
            "description": "Any MCP-compatible agent",
            "snippet": generic
        }
    });
    match agent {
        Some(name) => all.get(name).cloned().unwrap_or_else(|| json!({
            "error": format!("unknown agent {name:?}"),
            "valid": ["claude-code","opencode","cursor","nullclaw","omp","shell","generic-mcp"]
        })),
        None => all,
    }
}

/// FNV-1a 64-bit. Drawer source-ids are application-level identity
/// keys, not a security boundary, so a cheap non-crypto hash is fine.
/// Using `DefaultHasher` would be SipHash with a per-process seed →
/// different `web:<hash>` for the same URL across runs, which we
/// explicitly don't want.
fn short_hash(b: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    let mut s = String::with_capacity(16);
    for i in (0..16).rev() {
        let nibble = ((h >> (i * 4)) & 0xf) as u8;
        s.push(if nibble < 10 {
            (b'0' + nibble) as char
        } else {
            (b'a' + nibble - 10) as char
        });
    }
    s
}

/// Strip URLs out of `text` so we can decide whether the freeform
/// remainder is worth storing as its own drawer.
fn strip_urls(text: &str) -> String {
    let mut finder = crabcc_fetch::linkify::LinkFinder::new();
    finder.kinds(&[crabcc_fetch::linkify::LinkKind::Url]);
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for span in finder.spans(text) {
        if span.kind() == Some(&crabcc_fetch::linkify::LinkKind::Url) {
            out.push_str(&text[last..span.start()]);
            last = span.end();
        }
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    use crate::test_support::ensure_test_crabcc_home;

    #[test]
    fn auto_capture_inner_creates_drawer_with_session() {
        ensure_test_crabcc_home();
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
        ensure_test_crabcc_home();
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
        ensure_test_crabcc_home();
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
        ensure_test_crabcc_home();
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
    fn parse_before_timestamp_rfc3339_offset_applied() {
        // 2025-01-01T00:00:00Z == epoch 1735689600.
        // 2025-01-01T00:00:00-04:00 is midnight in UTC-4 == 04:00 UTC == +14400 s.
        let z = parse_before_timestamp("2025-01-01T00:00:00Z").unwrap();
        let neg4 = parse_before_timestamp("2025-01-01T00:00:00-04:00").unwrap();
        assert_eq!(neg4, z + 4 * 3_600, "-04:00 should yield 4h later in UTC");
        // +05:30 (IST) → 18:30 UTC on 2024-12-31 → 19800 s before midnight UTC.
        let pos530 = parse_before_timestamp("2025-01-01T00:00:00+05:30").unwrap();
        assert_eq!(pos530, z - (5 * 3_600 + 30 * 60), "+05:30 should yield 5h30m earlier in UTC");
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
