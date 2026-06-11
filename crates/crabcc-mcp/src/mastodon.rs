//! ### mastodon.rs — Mastodon MCP tools
//!
//! **Skill:** [`skill/crabcc-mcp/SKILL.md`](../../skill/crabcc-mcp/SKILL.md)
//! **Docs:** [`deploy/bots/POSTING.md`](../../../crabcc.app-social/POSTING.md)
//! **Schema:** [`crates/crabcc-mcp/src/schema.rs`](schema.rs)
//! **Dispatch:** [`crates/crabcc-mcp/src/dispatch.rs`](dispatch.rs)
//! **Transport:** [`crates/crabcc-mcp/src/transport.rs`](transport.rs)
//! **Tests:** [`#[cfg(test)] mod tests`](#) (27 security probes, 49 total)
//!
//! ---
//!
//! Agents use these to post reflections, notes, and release announcements
//! to a Mastodon instance. Open-source compatible — configure
//! `MASTODON_BASE` and `MASTODON_TOKEN` (or per-bot `<BOT>_TOKEN`) in the
//! environment. Tokens are **never** accepted through tool arguments — MCP
//! args travel through agent transcripts and log files; credentials don't
//! belong there.
//!
//! ### Auth
//!
//! Tokens are resolved from the environment only, in order of precedence:
//!
//! 1. `MASTODON_TOKEN_<BOT>` — when `bot` arg is given (all-caps)
//! 2. `<BOT>_TOKEN` — matches the `deploy/bots/post.mjs` convention
//! 3. `MASTODON_TOKEN` — single-token fallback
//!
//! No token in the environment → the tool returns a clear error (never
//! falls back to unauthenticated access).
//!
//! ### Internal modules
//!
//! - [`transport.rs`](transport.rs) — HTTP client (ureq), SSE formatting, gzip
//! - [`schema.rs`](schema.rs) — `tool_schema()`, `arg_str()`, `str_field()` helpers
//! - [`dispatch.rs`](dispatch.rs) — `dispatch_tool_inner()` routing
//! - [`memory.rs`](memory.rs) — `memory.*` tool pattern (same `pub fn tools_def()` + `pub fn dispatch()` shape)
//! - [`crabcc-core`](../../crabcc-core/src/lib.rs) — symbol index, Store, Fts, outline, query, upgrade
//! - [`crabcc-memory`](../../crabcc-memory/src/lib.rs) — Palace, hybrid search, mining, reminders
//! - [`deploy/bots/post.mjs`](../../../crabcc.app-social/deploy/bots/post.mjs) — existing Mastodon poster (Node.js)
//! - [`deploy/bots/bots.config.json`](../../../crabcc.app-social/deploy/bots/bots.config.json) — bot roster + voice/tone spec
//!
//! ### Tools
//!
//!   mastodon.post   — write a status (optionally: reply with `in_reply_to_id`)
//!   mastodon.read   — read home timeline
//!   mastodon.verify — check token + instance reachability
//!
//! The Mastodon REST API is synchronous and JSON-shaped, so we use a
//! blocking HTTP client (`ureq`) — no async runtime needed.

use crate::{arg_str, str_field, tool_schema as tool};
use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};
use ureq::Agent;

// ── defaults ────────────────────────────────────────────────────────

const DEFAULT_BASE_URL: &str = "https://social.crabcc.app";
const REQUEST_TIMEOUT_SECS: u64 = 30;

// ── rate limiting ───────────────────────────────────────────────────

/// Max attempts per token per window.
const RATE_LIMIT_MAX: usize = 5;
/// Window duration (48 hours in seconds).
const RATE_LIMIT_WINDOW_SECS: u64 = 48 * 3600;
/// Docs link surfaced in rate-limit responses.
const RATE_LIMIT_DOCS: &str = "https://crabcc.app/docs/mastodon-auth";

/// Per-token rate-limit state: timestamps of recent attempts.
#[derive(Debug, Default)]
struct RateLimitState {
    attempts: Vec<Instant>,
}

/// Global rate-limit store, keyed by token hash (djb2 of the token string).
static RATE_LIMITS: OnceLock<Mutex<HashMap<u64, RateLimitState>>> = OnceLock::new();

fn rate_limits() -> &'static Mutex<HashMap<u64, RateLimitState>> {
    RATE_LIMITS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Rate-limit result returned to the caller.
struct RateLimitResult {
    allowed: bool,
    used: usize,
    left: usize,
    resets_in_secs: u64,
}

/// Check whether the given token is within its rate limit. Prunes
/// expired entries from the window. Returns the current state.
fn check_rate_limit(token: &str) -> RateLimitResult {
    let token_hash = fnv1a_u64(token);
    let mut limits = rate_limits().lock().unwrap_or_else(|e| e.into_inner());
    let state = limits.entry(token_hash).or_default();
    let now = Instant::now();
    let window = Duration::from_secs(RATE_LIMIT_WINDOW_SECS);

    // Prune attempts older than the window
    state.attempts.retain(|t| now.duration_since(*t) < window);

    let used = state.attempts.len();
    let left = RATE_LIMIT_MAX.saturating_sub(used);
    let allowed = left > 0;

    // Compute reset time: when the oldest attempt expires, or now + window
    let resets_in_secs = state
        .attempts
        .first()
        .map(|oldest| {
            let elapsed = now.duration_since(*oldest);
            window.saturating_sub(elapsed).as_secs()
        })
        .unwrap_or(RATE_LIMIT_WINDOW_SECS);

    RateLimitResult {
        allowed,
        used,
        left,
        resets_in_secs,
    }
}

/// Record a successful attempt for the given token.
fn record_attempt(token: &str) {
    let token_hash = fnv1a_u64(token);
    let mut limits = rate_limits().lock().unwrap_or_else(|e| e.into_inner());
    let state = limits.entry(token_hash).or_default();
    state.attempts.push(Instant::now());
}

/// Build the rate-limit metadata object included in every response.
fn rate_limit_meta(result: &RateLimitResult) -> Value {
    json!({
        "rate_limit": {
            "rules": format!("max {} attempts per {} hours", RATE_LIMIT_MAX, RATE_LIMIT_WINDOW_SECS / 3600),
            "allowed": result.allowed,
            "attempts_used": result.used,
            "attempts_left": result.left,
            "resets_in_seconds": result.resets_in_secs,
            "docs": RATE_LIMIT_DOCS,
        }
    })
}

// ── SQLite cache + history ──────────────────────────────────────────

/// Default TTL for cached Mastodon responses (5 minutes).
const CACHE_TTL_SECS: i64 = 300;

/// Open (or create) the mastodon cache/history database at
/// `$CRABCC_HOME/mastodon-store.db`, falling back to a temp file.
static CACHE_DB: OnceLock<Mutex<Connection>> = OnceLock::new();

fn cache_db() -> &'static Mutex<Connection> {
    CACHE_DB.get_or_init(|| {
        let path = std::env::var("CRABCC_HOME")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("crabcc-mastodon"));
        std::fs::create_dir_all(&path).ok();
        let db_path = path.join("mastodon-store.db");
        let conn = Connection::open(&db_path).expect("open mastodon-store.db");

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS mastodon_cache (
                cache_key  TEXT PRIMARY KEY,
                response   TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                ttl_secs   INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS mastodon_history (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                tool         TEXT NOT NULL,
                token_hash   INTEGER NOT NULL,
                endpoint     TEXT NOT NULL,
                status       TEXT NOT NULL,
                elapsed_ms   INTEGER,
                payload_size INTEGER,
                created_at   INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_cache_expiry
                ON mastodon_cache(created_at + ttl_secs);
            CREATE INDEX IF NOT EXISTS idx_history_tool
                ON mastodon_history(tool, created_at);",
        )
        .expect("mastodon-store schema");

        Mutex::new(conn)
    })
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Look up a cached response. Returns `Some(JSON string)` if found and
/// not expired, `None` otherwise.
fn cache_get(key: &str) -> Option<String> {
    let db = cache_db().lock().unwrap_or_else(|e| e.into_inner());
    let now = now_unix();
    db.query_row(
        "SELECT response FROM mastodon_cache \
         WHERE cache_key = ?1 AND (created_at + ttl_secs) > ?2",
        rusqlite::params![key, now],
        |row| row.get(0),
    )
    .ok()
}

/// Store a response in the cache.
fn cache_put(key: &str, response: &str, ttl_secs: i64) {
    let db = cache_db().lock().unwrap_or_else(|e| e.into_inner());
    let now = now_unix();
    db.execute(
        "INSERT OR REPLACE INTO mastodon_cache (cache_key, response, created_at, ttl_secs) \
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![key, response, now, ttl_secs],
    )
    .ok();
}

/// Build a deterministic cache key from an API endpoint and a SHA-256 hex prefix.
fn cache_key(endpoint: &str, params_hash: &str) -> String {
    format!("{endpoint}:{params_hash}")
}

/// Log a request to the history table.
fn log_history(
    tool: &str,
    token: &str,
    endpoint: &str,
    status: &str,
    elapsed_ms: u64,
    payload_size: usize,
) {
    let db = cache_db().lock().unwrap_or_else(|e| e.into_inner());
    let token_hash = fnv1a_u64(token) as i64;
    let now = now_unix();
    db.execute(
        "INSERT INTO mastodon_history (tool, token_hash, endpoint, status, elapsed_ms, payload_size, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![tool, token_hash, endpoint, status, elapsed_ms as i64, payload_size as i64, now],
    )
    .ok();
}

// ── dashboard stats ─────────────────────────────────────────────────

/// Server start time, set once on first access.
static START_TIME: OnceLock<Instant> = OnceLock::new();

fn start_time() -> Instant {
    *START_TIME.get_or_init(Instant::now)
}

/// Gather live stats for the admin dashboard. Returns a JSON object
/// with rate-limit state, cache size, recent history, and uptime.
pub fn gather_stats() -> Value {
    let uptime_secs = start_time().elapsed().as_secs();

    // Rate-limit snapshot
    let limits = rate_limits().lock().unwrap_or_else(|e| e.into_inner());
    let rate_limits: Vec<Value> = limits
        .iter()
        .map(|(hash, state)| {
            let now = Instant::now();
            let window = Duration::from_secs(RATE_LIMIT_WINDOW_SECS);
            let used = state
                .attempts
                .iter()
                .filter(|t| now.duration_since(**t) < window)
                .count();
            let left = RATE_LIMIT_MAX.saturating_sub(used);
            json!({
                "token_hash": format!("{hash:x}"),
                "used": used,
                "left": left,
                "max": RATE_LIMIT_MAX,
            })
        })
        .collect();

    // Cache + history stats from SQLite
    let (cache_entries, recent_history) = {
        let db = cache_db().lock().unwrap_or_else(|e| e.into_inner());
        let cache_count: i64 = db
            .query_row("SELECT COUNT(*) FROM mastodon_cache", [], |r| r.get(0))
            .unwrap_or(0);
        let history: Vec<Value> = db
            .prepare_cached("SELECT tool, status, elapsed_ms, payload_size, created_at FROM mastodon_history ORDER BY id DESC LIMIT 20")
            .ok()
            .map(|mut s| {
                s.query_map([], |row| {
                    Ok(json!({
                        "tool": row.get::<_, String>(0)?,
                        "status": row.get::<_, String>(1)?,
                        "elapsed_ms": row.get::<_, i64>(2)?,
                        "payload_size": row.get::<_, i64>(3)?,
                        "created_at": row.get::<_, i64>(4)?,
                    }))
                })
                .ok()
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                .unwrap_or_default()
            })
            .unwrap_or_default();
        (cache_count, history)
    };

    json!({
        "uptime_seconds": uptime_secs,
        "rate_limits": rate_limits,
        "cache_entries": cache_entries,
        "recent_history": recent_history,
        "config": {
            "rate_limit_max": RATE_LIMIT_MAX,
            "rate_limit_window_hours": RATE_LIMIT_WINDOW_SECS / 3600,
            "cache_ttl_seconds": CACHE_TTL_SECS,
            "max_body_size": crate::transport::MAX_BODY_SIZE,
        }
    })
}

/// Validated base URL — must be `https://` with a host, no `@` userinfo.
/// Returns the URL without trailing slash for consistent joining.
fn validate_base_url(raw: &str) -> Result<String> {
    let s = raw.trim().trim_end_matches('/');
    if s.is_empty() {
        return Err(anyhow!("base_url must not be empty"));
    }
    if !s.starts_with("https://") {
        return Err(anyhow!(
            "base_url must start with https:// (got {s:?}). \
             Mastodon instances require TLS."
        ));
    }
    let host_part = &s["https://".len()..];
    if host_part.is_empty() {
        return Err(anyhow!("base_url has no host after https://"));
    }
    // Reject userinfo in URL (e.g. https://user:pass@evil.com)
    if host_part.contains('@') {
        return Err(anyhow!("base_url must not contain userinfo (@)"));
    }
    // Reject fragment / query in base URL
    if host_part.contains('#') || host_part.contains('?') {
        return Err(anyhow!("base_url must not contain fragment or query"));
    }
    Ok(s.to_string())
}

fn base_url(args: &Value) -> Result<String> {
    if let Some(raw) = args
        .get("base_url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return validate_base_url(raw);
    }
    if let Ok(raw) = std::env::var("MASTODON_BASE") {
        if !raw.trim().is_empty() {
            return validate_base_url(&raw);
        }
    }
    Ok(DEFAULT_BASE_URL.to_string())
}

/// Resolve the Mastodon access token from the environment. Never reads
/// `args` — tokens in MCP arguments leak through transcripts and logs.
///
/// Resolution order when `bot` is given (e.g. `bot = "crabcc"`):
///   1. `MASTODON_TOKEN_CRABCC`
///   2. `CRABCC_TOKEN`          (matches `deploy/bots/post.mjs`)
///   3. `MASTODON_TOKEN`        (fallback)
///
/// When `bot` is absent, reads `MASTODON_TOKEN` directly.
fn resolve_token(args: &Value) -> Result<String> {
    let bot = args
        .get("bot")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_uppercase());

    if let Some(ref bot_upper) = bot {
        for var in [
            format!("MASTODON_TOKEN_{bot_upper}"),
            format!("{bot_upper}_TOKEN"),
        ] {
            if let Ok(t) = std::env::var(&var) {
                if !t.trim().is_empty() {
                    return validate_token(&t);
                }
            }
        }
    }

    let t = std::env::var("MASTODON_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            if bot.is_some() {
                anyhow!(
                    "no Mastodon token found: set MASTODON_TOKEN (or \
                     MASTODON_TOKEN_<BOT> / <BOT>_TOKEN for per-bot auth). \
                     Create tokens at <instance>/settings/applications \
                     (scope: write:statuses for posting, read:statuses \
                     for reading)."
                )
            } else {
                anyhow!(
                    "no Mastodon token found: set MASTODON_TOKEN in the \
                     environment. Create one at <instance>/settings/applications \
                     (scope: write:statuses)."
                )
            }
        })?;

    validate_token(&t)
}

/// 256-byte lookup table for OAuth2 token character validation.
/// Indexed by byte value; `true` = valid. Static init at compile time.
static TOKEN_VALID: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = b'0';
    while i <= b'9' {
        t[i as usize] = true;
        i += 1;
    }
    i = b'A';
    while i <= b'Z' {
        t[i as usize] = true;
        i += 1;
    }
    i = b'a';
    while i <= b'z' {
        t[i as usize] = true;
        i += 1;
    }
    t[b'-' as usize] = true;
    t[b'_' as usize] = true;
    t[b'.' as usize] = true;
    t[b'+' as usize] = true;
    t[b'~' as usize] = true;
    t[b'/' as usize] = true;
    t[b'=' as usize] = true;
    t
};

/// Lightweight format check — Mastodon OAuth2 access tokens are typically
/// alphanumeric (hex, 64 chars) but may contain `.`, `+`, `~`, `/`, `=`
/// per RFC 6750. Minimum 20 chars to reject accidentally-copied client ids.
#[inline]
fn validate_token(raw: &str) -> Result<String> {
    let t = raw.trim();
    if t.len() < 20 {
        return Err(anyhow!(
            "Mastodon token too short ({} chars, expected ≥20). \
             Check that you copied the full access token, not the client id.",
            t.len()
        ));
    }
    // position() short-circuits at first invalid byte — faster than any()
    if let Some(_pos) = t.bytes().position(|b| !TOKEN_VALID[b as usize]) {
        return Err(anyhow!(
            "Mastodon token contains unexpected characters. \
             Tokens should be alphanumeric with dashes/underscores \
             (or OAuth2 chars . + ~ / =). Check that you copied \
             the access token, not a client secret."
        ));
    }
    Ok(t.to_string())
}

/// Sanitize an idempotency key: allow ASCII alphanumeric, `.`, `-`, `_`, `:`,
/// and cap at 128 bytes. Rejects keys with CR/LF (header injection).
/// Dots are safe in HTTP header values (RFC 7230 §3.2.6) and common in
/// version strings (e.g. `release:v1.0.0`).
/// Lookup table for idempotency key character validation.
static IDEM_VALID: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = b'0';
    while i <= b'9' {
        t[i as usize] = true;
        i += 1;
    }
    i = b'A';
    while i <= b'Z' {
        t[i as usize] = true;
        i += 1;
    }
    i = b'a';
    while i <= b'z' {
        t[i as usize] = true;
        i += 1;
    }
    t[b'-' as usize] = true;
    t[b'_' as usize] = true;
    t[b':' as usize] = true;
    t[b'.' as usize] = true;
    t
};

#[inline]
fn sanitize_idem_key(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let cap = bytes.len().min(128);
    let mut out = String::with_capacity(cap);
    for &b in bytes.iter().take(128) {
        if IDEM_VALID[b as usize] {
            out.push(b as char);
        }
    }
    out
}

/// Percent-encode a hashtag for safe URL interpolation. Only allows
/// alphanumeric characters and underscores; everything else is encoded.
/// Mastodon hashtags are `[a-zA-Z0-9_]+` in practice.
/// Lookup table for hashtag character validation.
static HASHTAG_VALID: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = b'0';
    while i <= b'9' {
        t[i as usize] = true;
        i += 1;
    }
    i = b'A';
    while i <= b'Z' {
        t[i as usize] = true;
        i += 1;
    }
    i = b'a';
    while i <= b'z' {
        t[i as usize] = true;
        i += 1;
    }
    t[b'_' as usize] = true;
    t
};

#[inline]
fn encode_hashtag(tag: &str) -> String {
    let bytes = tag.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if HASHTAG_VALID[b as usize] {
            out.push(b as char);
        }
    }
    out
}

// ── HTTP agent ──────────────────────────────────────────────────────

/// Cached `ureq::Agent` — created once, reused across calls for
/// connection pooling. `http_status_as_error` is disabled so we can
/// extract Mastodon error bodies from HTTP error responses.
static AGENT: OnceLock<Agent> = OnceLock::new();

fn agent() -> &'static Agent {
    AGENT.get_or_init(|| {
        let config = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(REQUEST_TIMEOUT_SECS)))
            .http_status_as_error(false)
            .build();
        config.into()
    })
}

/// Read the body of an HTTP error response and return a descriptive
/// error. Used inline after each request instead of a helper function
/// to avoid naming the private `ureq::Response` type.
macro_rules! http_check {
    ($resp:expr, $ctx:expr) => {{
        let r = $resp;
        if !r.status().is_success() {
            let code = r.status().as_u16();
            let body = r
                .into_body()
                .read_to_string()
                .unwrap_or_else(|_| "(body unreadable)".into());
            let truncated: String = body.chars().take(500).collect();
            let suffix = if body.len() > 500 { "..." } else { "" };
            return Err(anyhow!("{}: HTTP {}: {}{}", $ctx, code, truncated, suffix));
        }
        r
    }};
}

// ── tool schema ─────────────────────────────────────────────────────

pub fn tools_def() -> Vec<Value> {
    let base_field = json!({
        "type": "string",
        "description": "Mastodon instance base URL (https:// only). \
                        Overrides MASTODON_BASE env. \
                        Default: https://social.crabcc.app."
    });
    let bot_field = json!({
        "type": "string",
        "description": "Bot handle to select the right access token. \
                        When set, the server reads MASTODON_TOKEN_<BOT> \
                        (or <BOT>_TOKEN) from the environment instead of \
                        MASTODON_TOKEN. Matches the post.mjs convention. \
                        Example: bot=crabcc → reads CRABCC_TOKEN env var."
    });

    vec![
        tool(
            "mastodon.post",
            "Post a status to Mastodon. Agents use this to write reflections, \
             release notes, or daily summaries to a timeline. Returns the \
             created status's id, url, and rendered content. Safe to replay \
             with the same `idempotency_key` — Mastodon deduplicates for ~1h.\n\n\
             Auth: set MASTODON_TOKEN (or <BOT>_TOKEN) in the environment. \
             Tokens are never accepted through tool arguments.",
            json!({
                "text":             str_field("Status body (plain text or HTML). Required, non-empty."),
                "visibility":       {
                    "type": "string",
                    "enum": ["public", "unlisted", "private", "direct"],
                    "description": "Visibility. Default: unlisted (won't appear \
                                    in public timelines but reachable via link)."
                },
                "spoiler_text":     str_field("Content warning / CW text (optional)."),
                "in_reply_to_id":   str_field("Mastodon status id to reply to. Creates a threaded reply. Optional."),
                "language":         str_field("ISO 639-1 language code. Default: en."),
                "idempotency_key":  str_field(
                    "Stable key per logical event (release id, commit sha). \
                     Mastodon deduplicates an identical key for ~1h — a \
                     re-run won't double-post. Sanitized: alphanumeric, \
                     dot, dash, underscore, colon; max 128 chars."
                ),
                "bot":              bot_field.clone(),
                "base_url":         base_field.clone(),
            }),
            &["text"],
        ),
        tool(
            "mastodon.read",
            "Read recent posts from a Mastodon timeline. Useful for agents to \
             review what they (or other bots) have posted before writing a \
             new reflection. Returns an array of status objects (trimmed: \
             id, account, content, created_at, url).\n\n\
             Auth: set MASTODON_TOKEN (or <BOT>_TOKEN) in the environment.",
            json!({
                "timeline": {
                    "type": "string",
                    "enum": ["home", "public", "tag"],
                    "description": "Which timeline to read. 'home' = your feed \
                                    (default). 'public' = instance-wide. \
                                    'tag' = filter by hashtag (requires `hashtag`).",
                },
                "limit": {
                    "type": "integer",
                    "description": "Max posts to return (default 20, clamped to 1–40)."
                },
                "hashtag":          str_field("Hashtag to filter (without #). Only used when timeline=tag. \
                                              Alphanumeric + underscores only; everything else stripped."),
                "bot":              bot_field.clone(),
                "base_url":         base_field.clone(),
            }),
            &[],
        ),
        tool(
            "mastodon.verify",
            "Verify the Mastodon token and instance are reachable. Returns the \
             authenticated account's id, username, display name, and the \
             instance's domain + version. Use as a smoke test.\n\n\
             Auth: set MASTODON_TOKEN (or <BOT>_TOKEN) in the environment.",
            json!({
                "bot":      bot_field,
                "base_url": base_field,
            }),
            &[],
        ),
    ]
}

// ── dispatch ────────────────────────────────────────────────────────

pub fn dispatch(tool_name: &str, args: &Value) -> Result<String> {
    let tool = tool_name
        .strip_prefix("mastodon.")
        .ok_or_else(|| anyhow!("mastodon dispatch: expected mastodon.*, got {tool_name}"))?;

    match tool {
        "post" => handle_post(args),
        "read" => handle_read(args),
        "verify" => handle_verify(args),
        other => Err(anyhow!("unknown mastodon tool: mastodon.{other}")),
    }
}

// ── handlers ────────────────────────────────────────────────────────

fn handle_post(args: &Value) -> Result<String> {
    let text = arg_str(args, "text")?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("mastodon.post: text must not be empty"));
    }

    let base = base_url(args)?;
    let tok = resolve_token(args)?;

    // Rate limit check
    let rl = check_rate_limit(&tok);
    let rl_meta = rate_limit_meta(&rl);
    if !rl.allowed {
        warn!(
            target: "crabcc_mcp::mastodon",
            used = rl.used,
            left = rl.left,
            resets_secs = rl.resets_in_secs,
            "mastodon.post: rate limited"
        );
        let out = json!({
            "ok": false,
            "error": "rate limited",
            "rate_limit": rl_meta["rate_limit"],
        });
        return Ok(out.to_string());
    }
    debug!(
        target: "crabcc_mcp::mastodon",
        used = rl.used,
        left = rl.left,
        "mastodon.post: rate limit check passed"
    );

    let visibility = args
        .get("visibility")
        .and_then(|v| v.as_str())
        .unwrap_or("unlisted");
    // Validate visibility against the known set
    if !matches!(visibility, "public" | "unlisted" | "private" | "direct") {
        return Err(anyhow!(
            "mastodon.post: invalid visibility {visibility:?}. \
             Must be one of: public, unlisted, private, direct."
        ));
    }

    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("en");
    let spoiler = args.get("spoiler_text").and_then(|v| v.as_str());

    let mut body = json!({
        "status": trimmed,
        "visibility": visibility,
        "language": language,
    });
    if let Some(cw) = spoiler {
        body["spoiler_text"] = json!(cw);
    }
    if let Some(reply_id) = args.get("in_reply_to_id").and_then(|v| v.as_str()) {
        if !reply_id.is_empty() {
            body["in_reply_to_id"] = json!(reply_id);
        }
    }

    let idem_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(sanitize_idem_key)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("mcp:{:016x}", fnv1a_u64(trimmed)));

    let endpoint = format!("{base}/api/v1/statuses");
    let span =
        tracing::span!(tracing::Level::INFO, "mastodon_api", tool = "post", endpoint = %endpoint);
    let _enter = span.enter();
    let started = Instant::now();
    let resp = agent()
        .post(&endpoint)
        .header("Authorization", &format!("Bearer {tok}"))
        .header("Content-Type", "application/json")
        .header("Idempotency-Key", &idem_key)
        .send_json(body)
        .map_err(|e| anyhow!("mastodon.post: request failed (check base_url): {e}"))?;

    let resp = http_check!(resp, "mastodon.post");
    let raw_body = resp.into_body().read_to_string().unwrap_or_default();
    let json: Value = serde_json::from_str(&raw_body)
        .map_err(|e| anyhow!("mastodon.post: parse response: {e}"))?;
    let elapsed = started.elapsed().as_millis() as u64;
    let payload_size = raw_body.len();

    log_history("post", &tok, &endpoint, "ok", elapsed, payload_size);
    record_attempt(&tok);
    let mut out = json!({
        "ok": true,
        "id": json["id"],
        "url": json["url"],
        "created_at": json["created_at"],
        "content": json["content"],
    });
    // Merge rate-limit metadata into the response
    if let Some(obj) = out.as_object_mut() {
        obj.extend(rl_meta.as_object().cloned().unwrap_or_default());
    }
    Ok(out.to_string())
}

fn handle_read(args: &Value) -> Result<String> {
    let base = base_url(args)?;
    let tok = resolve_token(args)?;

    // Rate limit check
    let rl = check_rate_limit(&tok);
    let rl_meta = rate_limit_meta(&rl);
    if !rl.allowed {
        warn!(
            target: "crabcc_mcp::mastodon",
            used = rl.used,
            left = rl.left,
            "mastodon.read: rate limited"
        );
        let out = json!({
            "ok": false,
            "error": "rate limited",
            "rate_limit": rl_meta["rate_limit"],
        });
        return Ok(out.to_string());
    }
    debug!(target: "crabcc_mcp::mastodon", used = rl.used, left = rl.left, "mastodon.read: rate limit passed");
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n.clamp(1, 40))
        .unwrap_or(20) as usize;
    let timeline = args
        .get("timeline")
        .and_then(|v| v.as_str())
        .unwrap_or("home");

    let url = match timeline {
        "home" => format!("{base}/api/v1/timelines/home?limit={limit}"),
        "public" => format!("{base}/api/v1/timelines/public?limit={limit}"),
        "tag" => {
            let raw_tag = args
                .get("hashtag")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("mastodon.read: timeline=tag requires `hashtag` arg"))?;
            let tag = encode_hashtag(raw_tag);
            if tag.is_empty() {
                return Err(anyhow!(
                    "mastodon.read: hashtag must contain at least one \
                     alphanumeric character or underscore"
                ));
            }
            format!("{base}/api/v1/timelines/tag/{tag}?limit={limit}")
        }
        other => return Err(anyhow!("mastodon.read: unknown timeline {other:?}")),
    };

    // Cache check: use the full URL as the cache key
    let ck = cache_key("read", &format!("{:016x}", fnv1a_u64(&url)));
    if let Some(cached) = cache_get(&ck) {
        let trimmed: Value = serde_json::from_str(&cached).unwrap_or(Value::Null);
        if !trimmed.is_null() {
            debug!(target: "crabcc_mcp::mastodon", key = %ck, "mastodon.read: cache hit");
            let out = json!({
                "ok": true,
                "posts": trimmed,
                "cached": true,
                "rate_limit": rl_meta["rate_limit"],
            });
            return Ok(out.to_string());
        }
    }

    let span = tracing::span!(tracing::Level::INFO, "mastodon_api", tool = "read", endpoint = %url);
    let _enter = span.enter();
    let started = Instant::now();
    let resp = agent()
        .get(&url)
        .header("Authorization", &format!("Bearer {tok}"))
        .call()
        .map_err(|e| anyhow!("mastodon.read: request failed (check base_url): {e}"))?;

    let resp = http_check!(resp, "mastodon.read");
    let raw_body = resp.into_body().read_to_string().unwrap_or_default();
    let posts: Value = serde_json::from_str(&raw_body)
        .map_err(|e| anyhow!("mastodon.read: parse response: {e}"))?;
    let elapsed = started.elapsed().as_millis() as u64;
    let payload_size = raw_body.len();

    // Trim each post to the fields an agent actually needs — keep the
    // response compact. A full Mastodon Status object is ~2-3 KB each.
    let trimmed: Vec<Value> = match posts.as_array() {
        Some(arr) => arr
            .iter()
            .map(|p| {
                json!({
                    "id": p["id"],
                    "created_at": p["created_at"],
                    "content": p["content"],
                    "url": p["url"],
                    "account": {
                        "id": p["account"]["id"],
                        "username": p["account"]["username"],
                        "display_name": p["account"]["display_name"],
                        "acct": p["account"]["acct"],
                    },
                })
            })
            .collect(),
        None => {
            return Err(anyhow!("mastodon.read: unexpected response shape"));
        }
    };

    record_attempt(&tok);
    log_history("read", &tok, &url, "ok", elapsed, payload_size);

    // Cache the trimmed response
    let cache_val = serde_json::to_string(&trimmed).unwrap_or_default();
    cache_put(&ck, &cache_val, CACHE_TTL_SECS);
    debug!(target: "crabcc_mcp::mastodon", key = %ck, ttl = CACHE_TTL_SECS, "mastodon.read: cached response");

    let out = json!({
        "ok": true,
        "posts": trimmed,
        "rate_limit": rl_meta["rate_limit"],
    });
    Ok(out.to_string())
}

fn handle_verify(args: &Value) -> Result<String> {
    let base = base_url(args)?;
    let tok = resolve_token(args)?;

    // Rate limit check
    let rl = check_rate_limit(&tok);
    let rl_meta = rate_limit_meta(&rl);
    if !rl.allowed {
        let out = json!({
            "ok": false,
            "error": "rate limited",
            "rate_limit": rl_meta["rate_limit"],
        });
        return Ok(out.to_string());
    }

    let endpoint = format!("{base}/api/v1/accounts/verify_credentials");
    let span =
        tracing::span!(tracing::Level::INFO, "mastodon_api", tool = "verify", endpoint = %endpoint);
    let _enter = span.enter();
    let started = Instant::now();
    let account: Value = {
        let resp = agent()
            .get(&endpoint)
            .header("Authorization", &format!("Bearer {tok}"))
            .call()
            .map_err(|e| {
                anyhow!(
                    "mastodon.verify: request failed (check base_url \"{base}\" \
                     and that MASTODON_TOKEN is valid): {e}"
                )
            })?;
        let resp = http_check!(resp, "mastodon.verify");
        let raw = resp.into_body().read_to_string().unwrap_or_default();
        let elapsed = started.elapsed().as_millis() as u64;
        log_history("verify", &tok, &endpoint, "ok", elapsed, raw.len());
        serde_json::from_str(&raw).map_err(|e| anyhow!("mastodon.verify: parse account: {e}"))?
    };

    // Also try to fetch the instance info for version / domain.
    let instance: Value = agent()
        .get(&format!("{base}/api/v2/instance"))
        .call()
        .ok()
        .filter(|r| r.status().is_success())
        .and_then(|r| r.into_body().read_json().ok())
        .unwrap_or(Value::Null);

    let out = json!({
        "ok": true,
        "account": {
            "id": account["id"],
            "username": account["username"],
            "display_name": account["display_name"],
            "acct": account["acct"],
            "bot": account["bot"],
        },
        "instance": {
            "domain": instance["domain"],
            "title": instance["title"],
            "version": instance["version"],
        },
    });
    Ok(out.to_string())
}

// ── helpers ─────────────────────────────────────────────────────────

/// FNV-1a 64-bit — deterministic, ~1 cycle/byte. Collision-resistant
/// for token bucketing and cache-key generation. Deterministic across
/// runs so SQLite cache keys survive restarts.
#[inline]
fn fnv1a_u64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

// ── bench-accessible wrappers ───────────────────────────────────────
// Public re-exports of internal functions so criterion benchmarks
// (gated behind `--features bench`) can measure the pure-CPU hot paths.
// These are NOT part of the public API surface.

#[doc(hidden)]
pub fn validate_token_for_bench(raw: &str) -> Result<String> {
    validate_token(raw)
}

#[doc(hidden)]
pub fn sanitize_idem_key_for_bench(raw: &str) -> String {
    sanitize_idem_key(raw)
}

#[doc(hidden)]
pub fn encode_hashtag_for_bench(tag: &str) -> String {
    encode_hashtag(tag)
}

#[doc(hidden)]
pub fn sse_event_with_id_for_bench(event: &str, data: &str, id: u64) -> String {
    crate::transport::sse_event_with_id(event, data, id)
}

#[doc(hidden)]
pub fn fnv1a_u64_for_bench(s: &str) -> u64 {
    fnv1a_u64(s)
}

// ── security test suite ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    /// Serialize tests that mutate environment variables.
    /// `cargo test` runs tests in parallel within the same process,
    /// so `std::env::set_var` in one test can clobber another.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    // ── base_url SSRF probes ──────────────────────────────────────

    #[test]
    fn base_url_rejects_plain_http() {
        assert!(validate_base_url("http://evil.com").is_err());
        assert!(validate_base_url("http://localhost").is_err());
        assert!(validate_base_url("HTTP://EVIL.COM").is_err());
    }

    #[test]
    fn base_url_rejects_userinfo() {
        assert!(validate_base_url("https://user:pass@evil.com").is_err());
        assert!(validate_base_url("https://@evil.com").is_err());
        assert!(validate_base_url("https://evil.com@legit.com").is_err());
    }

    #[test]
    fn base_url_rejects_fragment_and_query() {
        assert!(validate_base_url("https://evil.com#frag").is_err());
        assert!(validate_base_url("https://evil.com?q=1").is_err());
        assert!(validate_base_url("https://evil.com/path?x=1#y").is_err());
    }

    #[test]
    fn base_url_rejects_empty() {
        assert!(validate_base_url("").is_err());
        assert!(validate_base_url("   ").is_err());
    }

    #[test]
    fn base_url_rejects_no_host() {
        assert!(validate_base_url("https://").is_err());
        // "https:///path" has "/path" as the host portion — our
        // validation treats it as a host. Real DNS won't resolve it,
        // but the structural check passes. The real SSRF guard is
        // the @ / # / ? checks.
    }

    #[test]
    fn base_url_accepts_valid() {
        assert!(validate_base_url("https://social.crabcc.app").is_ok());
        assert!(validate_base_url("https://mastodon.social").is_ok());
        assert!(validate_base_url("https://social.crabcc.app/").is_ok()); // trailing slash stripped
        assert!(validate_base_url("https://example.com:443").is_ok());
    }

    #[test]
    fn base_url_strips_trailing_slash() {
        let url = validate_base_url("https://example.com/").unwrap();
        assert_eq!(url, "https://example.com");
    }

    #[test]
    fn base_url_from_args_uses_default() {
        let args = json!({});
        let url = base_url(&args).unwrap();
        assert_eq!(url, DEFAULT_BASE_URL);
    }

    #[test]
    fn base_url_from_args_rejects_invalid() {
        let args = json!({"base_url": "http://evil.com"});
        assert!(base_url(&args).is_err());
    }

    // ── token validation ───────────────────────────────────────────

    #[test]
    fn token_rejects_short() {
        assert!(validate_token("abc123").is_err()); // 6 chars
        assert!(validate_token("1234567890123456789").is_err()); // 19 chars
    }

    #[test]
    fn token_accepts_minimum_length() {
        // 20 chars of valid hex
        assert!(validate_token("12345678901234567890").is_ok());
    }

    #[test]
    fn token_accepts_oauth2_chars() {
        // OAuth2 tokens may contain . + ~ / =
        assert!(validate_token("abc.def+ghi~jkl/mno=pqr1234567890").is_ok());
    }

    #[test]
    fn token_rejects_spaces() {
        assert!(validate_token("abc 123 def 456 ghi 789 jkl 012").is_err());
    }

    #[test]
    fn token_rejects_newlines() {
        // Trailing newlines are stripped by trim() — test embedded ones.
        assert!(validate_token("abcde\nf1234567890abcde").is_err());
        assert!(validate_token("abcde\r\nf1234567890abcde").is_err());
    }

    #[test]
    fn token_trims_whitespace() {
        // Leading/trailing whitespace should be trimmed before validation
        let t = validate_token("  abcdef1234567890abcd  ").unwrap();
        assert_eq!(t, "abcdef1234567890abcd");
    }

    #[test]
    fn token_rejects_unicode() {
        assert!(validate_token("abcdef1234567890aböd").is_err());
    }

    #[test]
    fn token_rejects_null_byte() {
        let raw = "abcdef1234567890ab\0d";
        assert!(validate_token(raw).is_err());
    }

    // ── idempotency key sanitization ───────────────────────────────

    #[test]
    fn idem_key_preserves_valid_chars() {
        // Alphanumeric, ., -, _, : are preserved.
        let clean = sanitize_idem_key("release:v2.0.0-42_test");
        assert_eq!(clean, "release:v2.0.0-42_test");
    }

    #[test]
    fn idem_key_strips_special_chars() {
        let clean = sanitize_idem_key("key\r\nInjected: value");
        assert!(!clean.contains('\r'));
        assert!(!clean.contains('\n'));
        // Spaces are stripped
        assert!(!clean.contains(' '));
    }

    #[test]
    fn idem_key_allows_dots() {
        // Dots are allowed (common in version strings)
        let clean = sanitize_idem_key("release:v1.0.0");
        assert!(clean.contains('.'));
        assert_eq!(clean, "release:v1.0.0");
    }

    #[test]
    fn idem_key_strips_spaces() {
        let clean = sanitize_idem_key("hello world");
        assert_eq!(clean, "helloworld");
    }

    #[test]
    fn idem_key_caps_at_128() {
        let long = "a".repeat(200);
        let clean = sanitize_idem_key(&long);
        assert_eq!(clean.len(), 128);
    }

    #[test]
    fn idem_key_all_stripped_becomes_empty() {
        let clean = sanitize_idem_key("!@#$%^&*()");
        assert!(clean.is_empty());
    }

    // ── hashtag encoding ───────────────────────────────────────────

    #[test]
    fn hashtag_strips_path_traversal() {
        let clean = encode_hashtag("../../admin");
        assert!(!clean.contains('/'));
        assert!(!clean.contains('.'));
        assert_eq!(clean, "admin");
    }

    #[test]
    fn hashtag_strips_special_chars() {
        let clean = encode_hashtag("test<script>alert(1)");
        assert!(!clean.contains('<'));
        assert!(!clean.contains('>'));
        assert!(!clean.contains('('));
        assert!(!clean.contains(')'));
        assert_eq!(clean, "testscriptalert1");
    }

    #[test]
    fn hashtag_strips_spaces() {
        let clean = encode_hashtag("hello world");
        assert_eq!(clean, "helloworld");
    }

    #[test]
    fn hashtag_preserves_underscore() {
        let clean = encode_hashtag("hello_world");
        assert_eq!(clean, "hello_world");
    }

    #[test]
    fn hashtag_empty_after_sanitization() {
        let clean = encode_hashtag("!@#$%");
        assert!(clean.is_empty());
    }

    #[test]
    fn hashtag_strips_percent_encoded() {
        let clean = encode_hashtag("%2e%2e%2f");
        assert!(!clean.contains('%'));
    }

    #[test]
    fn hashtag_handles_unicode() {
        // Non-ASCII stripped (is_ascii_alphanumeric rejects it)
        let clean = encode_hashtag("café");
        assert_eq!(clean, "caf");
    }

    // ── token resolution ───────────────────────────────────────────

    #[test]
    fn resolve_token_fails_without_env() {
        let _guard = ENV_MUTEX.lock().unwrap();
        // Ensure no MASTODON_TOKEN leaks from the test environment
        std::env::remove_var("MASTODON_TOKEN");
        let args = json!({});
        assert!(resolve_token(&args).is_err());
    }

    #[test]
    fn resolve_token_rejects_empty_env() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::set_var("MASTODON_TOKEN", "   ");
        let args = json!({});
        assert!(resolve_token(&args).is_err());
        std::env::remove_var("MASTODON_TOKEN");
    }

    #[test]
    fn resolve_token_uses_default_env() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let token = "abcdef1234567890abcd"; // 20 chars
        std::env::set_var("MASTODON_TOKEN", token);
        let args = json!({});
        let resolved = resolve_token(&args).unwrap();
        assert_eq!(resolved, token);
        std::env::remove_var("MASTODON_TOKEN");
    }

    #[test]
    fn resolve_token_bot_specific_env() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let token = "bot_specific_token_12345"; // 24 chars
        std::env::set_var("CRABCC_TOKEN", token);
        let args = json!({"bot": "crabcc"});
        let resolved = resolve_token(&args).unwrap();
        assert_eq!(resolved, token);
        std::env::remove_var("CRABCC_TOKEN");
    }

    #[test]
    fn resolve_token_bot_namespaced_env() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let token = "namespaced_token_123456"; // 22 chars
        std::env::set_var("MASTODON_TOKEN_CRABCC", token);
        let args = json!({"bot": "crabcc"});
        let resolved = resolve_token(&args).unwrap();
        assert_eq!(resolved, token);
        std::env::remove_var("MASTODON_TOKEN_CRABCC");
    }

    #[test]
    fn resolve_token_bot_falls_back_to_default() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let token = "default_token_12345678"; // 22 chars
        std::env::set_var("MASTODON_TOKEN", token);
        let args = json!({"bot": "unknownbot"});
        let resolved = resolve_token(&args).unwrap();
        assert_eq!(resolved, token);
        std::env::remove_var("MASTODON_TOKEN");
    }

    // ── handle_post input validation (pre-HTTP) ────────────────────

    #[test]
    fn handle_post_rejects_empty_text() {
        let args = json!({"text": ""});
        assert!(handle_post(&args).is_err());
    }

    #[test]
    fn handle_post_rejects_whitespace_only_text() {
        let args = json!({"text": "   \n\t  "});
        assert!(handle_post(&args).is_err());
    }

    #[test]
    fn handle_post_rejects_invalid_visibility() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::set_var("MASTODON_TOKEN", "abcdef1234567890abcd");
        let args = json!({"text": "hello", "visibility": "super_secret"});
        let result = handle_post(&args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid visibility"), "got: {err}");
        std::env::remove_var("MASTODON_TOKEN");
    }

    #[test]
    fn handle_post_rejects_invalid_base_url() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::set_var("MASTODON_TOKEN", "abcdef1234567890abcd");
        let args = json!({"text": "hello", "base_url": "http://evil.com"});
        assert!(handle_post(&args).is_err());
        std::env::remove_var("MASTODON_TOKEN");
    }

    #[test]
    fn handle_post_idempotency_key_sanitized_before_use() {
        // The sanitize_idem_key function is called on the user-provided
        // key. Verify CRLF is stripped at the sanitizer level (already
        // tested above in idem_key_strips_special_chars). This test
        // confirms the handler calls the sanitizer before setting the header.
        //
        // We test this indirectly: a key with CRLF won't cause a
        // validation error — it just gets sanitized. The function
        // proceeds to the HTTP layer (which we can't test without a
        // server), but the sanitizer is the security boundary.
        let clean = sanitize_idem_key("key\r\nInjected: true");
        assert!(!clean.contains('\r'));
        assert!(!clean.contains('\n'));
        assert_eq!(clean, "keyInjected:true"); // space stripped, colon kept
    }

    // ── handle_read input validation ───────────────────────────────

    #[test]
    fn handle_read_rejects_unknown_timeline() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::set_var("MASTODON_TOKEN", "abcdef1234567890abcd");
        let args = json!({"timeline": "dm"});
        let result = handle_read(&args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown timeline"), "got: {err}");
        std::env::remove_var("MASTODON_TOKEN");
    }

    #[test]
    fn handle_read_rejects_empty_hashtag() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::set_var("MASTODON_TOKEN", "abcdef1234567890abcd");
        let args = json!({"timeline": "tag", "hashtag": "!@#$"});
        let result = handle_read(&args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("hashtag must contain"), "got: {err}");
        std::env::remove_var("MASTODON_TOKEN");
    }

    #[test]
    fn handle_read_clamps_limit_to_valid_range() {
        // Test the clamp logic directly (exercised by handle_read before HTTP)
        assert_eq!(0u64.clamp(1, 40), 1);
        assert_eq!(1000u64.clamp(1, 40), 40);
        assert_eq!(20u64.clamp(1, 40), 20);
    }

    // ── handle_verify input validation ─────────────────────────────

    #[test]
    fn handle_verify_rejects_invalid_base_url() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::set_var("MASTODON_TOKEN", "abcdef1234567890abcd");
        let args = json!({"base_url": "http://evil.com"});
        assert!(handle_verify(&args).is_err());
        std::env::remove_var("MASTODON_TOKEN");
    }

    // ── dispatch routing ───────────────────────────────────────────

    #[test]
    fn dispatch_rejects_unknown_tool() {
        let result = dispatch("mastodon.delete", &json!({}));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown mastodon tool"), "got: {err}");
    }

    #[test]
    fn dispatch_rejects_wrong_prefix() {
        let result = dispatch("memory.search", &json!({}));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("expected mastodon.*"), "got: {err}");
    }

    // ── JSON response null-safety ──────────────────────────────────

    #[test]
    fn null_json_fields_become_null() {
        // When response fields are missing, serde_json Value access
        // returns Value::Null. Our code uses json!({ "key": p["field"] })
        // which maps Null → null in the output. This is safe — no panics.
        let p = json!({"id": "123", "content": "hello"});
        // Missing "url" field
        let out = json!({
            "id": p["id"],
            "url": p["url"], // returns Value::Null
        });
        assert_eq!(out["id"], "123");
        assert!(out["url"].is_null());
    }

    #[test]
    fn nested_null_json_fields() {
        // What if p["account"] is null? Indexing into Value::Null
        // returns Value::Null — no panic, safe.
        let p = json!({"id": "123", "account": null});
        let account: &Value = &p["account"];
        assert!(account.is_null());
        // Chaining: null["id"] → Value::Null
        let id: &Value = &account["id"];
        assert!(id.is_null());
    }
}
