//! Thin ntfy push notification helper.
//!
//! Fires a best-effort POST to a ntfy server on significant MCP events
//! (index builds, write_file, errors). Silently no-ops when `NTFY_URL`
//! is not set — no configuration required for the common case.
//!
//! ### Environment variables
//!
//!   NTFY_URL    — base URL of the ntfy server, e.g. https://ntfy.crabcc.app
//!   NTFY_TOPIC  — topic name (default: "crabcc")
//!   NTFY_USER   — username for HTTP Basic auth (optional)
//!   NTFY_TOKEN  — password / access token (optional)
//!
//! If only NTFY_TOKEN is set (no NTFY_USER), it is sent as a Bearer token.
//! If both are set, Basic auth is used.

use std::sync::OnceLock;
use std::time::Duration;
use ureq::Agent;

struct NtfyConfig {
    endpoint: String,
    user: Option<String>,
    token: Option<String>,
}

static CONFIG: OnceLock<Option<NtfyConfig>> = OnceLock::new();
static AGENT: OnceLock<Agent> = OnceLock::new();

fn agent() -> &'static Agent {
    AGENT.get_or_init(|| {
        Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(8)))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

fn config() -> Option<&'static NtfyConfig> {
    CONFIG
        .get_or_init(|| {
            let base = std::env::var("NTFY_URL").ok()?;
            let base = base.trim_end_matches('/');
            let topic = std::env::var("NTFY_TOPIC").unwrap_or_else(|_| "crabcc".to_string());
            let endpoint = format!("{base}/{topic}");
            let user = std::env::var("NTFY_USER").ok().filter(|s| !s.is_empty());
            let token = std::env::var("NTFY_TOKEN").ok().filter(|s| !s.is_empty());
            Some(NtfyConfig {
                endpoint,
                user,
                token,
            })
        })
        .as_ref()
}

/// Fire a best-effort ntfy notification. Silently drops errors —
/// notification failures must never crash or stall the MCP server.
pub fn push(title: &str, message: &str, tags: &str) {
    let Some(cfg) = config() else { return };

    let mut req = agent()
        .post(&cfg.endpoint)
        .header("Title", title)
        .header("Tags", tags)
        .header("Content-Type", "text/plain");

    req = match (&cfg.user, &cfg.token) {
        (Some(user), Some(token)) => {
            let raw = format!("{user}:{token}");
            let encoded = base64_encode(raw.as_bytes());
            req.header("Authorization", &format!("Basic {encoded}"))
        }
        (None, Some(token)) => req.header("Authorization", &format!("Bearer {token}")),
        _ => req,
    };

    if let Err(e) = req.send(message) {
        tracing::debug!(target: "crabcc_mcp::ntfy", error = %e, "ntfy push failed");
    }
}

/// Notify after a successful index build.
pub fn on_index(symbols: usize, elapsed_ms: u64) {
    push(
        "crabcc indexed",
        &format!("{symbols} symbols in {elapsed_ms}ms"),
        "file_cabinet",
    );
}

/// Notify after a refresh (delta or full).
pub fn on_refresh(updated: usize, elapsed_ms: u64) {
    push(
        "crabcc refresh",
        &format!("{updated} files updated in {elapsed_ms}ms"),
        "arrows_counterclockwise",
    );
}

/// Notify after write_file.
pub fn on_write(path: &str, bytes: usize) {
    push(
        "crabcc write_file",
        &format!("wrote {path} ({bytes}B)"),
        "pencil",
    );
}

// ── base64 (no external dep) ────────────────────────────────────────

fn base64_encode(input: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[(n >> 18) & 0x3f] as char);
        out.push(CHARS[(n >> 12) & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            CHARS[(n >> 6) & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            CHARS[n & 0x3f] as char
        } else {
            '='
        });
    }
    out
}
