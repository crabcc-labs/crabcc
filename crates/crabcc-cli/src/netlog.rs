//! Outbound HTTP egress logging + allowlist enforcement (issue #160).
//!
//! This is the Phase 1 *mechanism*: infra HTTP clients (morph, telemetry,
//! `crabcc upgrade`, …) call [`guard`] before firing a request. It emits a
//! `tracing` event for the host and — in the default deny mode — blocks any
//! host not on the embedded [allowlist](netlog_allowlist.txt) with a clear
//! error before the request leaves the machine. A compromised dependency that
//! tries to phone home to an unlisted host fails loudly instead of silently
//! exfiltrating.
//!
//! Deliberately **not** applied to the web crawler (`crabcc-fetch`): that
//! reaches arbitrary user-specified hosts by design.
//!
//! Mode is read from `CRABCC_NETLOG_DENY`:
//!   * unset / `1` → **deny** unlisted hosts (default)
//!   * `0`         → log-only (record, never block — Phase 1 collection mode)
//!   * `audit`     → log-only, but `warn!` on each unlisted host for review
//!
//! Rollout note: only the `morph` client is wired so far. Before flipping more
//! callers to enforce, run them under `CRABCC_NETLOG_DENY=audit` to confirm the
//! allowlist is complete.

use anyhow::{bail, Result};
use std::collections::HashSet;
use std::sync::OnceLock;

/// Seed allowlist, compiled into the binary so enforcement never depends on a
/// file being present at runtime.
const SEED_ALLOWLIST: &str = include_str!("netlog_allowlist.txt");

/// Enforcement mode, from `CRABCC_NETLOG_DENY`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Block unlisted hosts (default).
    Deny,
    /// Record only, never block.
    LogOnly,
    /// Record only, but warn on unlisted hosts for later review.
    Audit,
}

impl Mode {
    /// Resolve the mode from the environment.
    pub fn from_env() -> Self {
        Self::from_raw(std::env::var("CRABCC_NETLOG_DENY").ok().as_deref())
    }

    /// Pure mapping of the raw env value to a mode (testable without env).
    fn from_raw(v: Option<&str>) -> Self {
        match v {
            Some("0") => Mode::LogOnly,
            Some("audit") | Some("AUDIT") => Mode::Audit,
            _ => Mode::Deny, // unset or "1" (or anything else) → deny
        }
    }
}

/// A parsed host allowlist: exact hosts plus subdomain suffixes.
#[derive(Debug, Default)]
pub struct Allowlist {
    exact: HashSet<String>,
    /// Suffixes like `.github.com` — match any subdomain and the bare apex.
    suffixes: Vec<String>,
}

impl Allowlist {
    /// Parse the line-oriented allowlist format (`#` comments, `*.x`/`.x`
    /// wildcards, one host per line, case-insensitive).
    pub fn parse(text: &str) -> Self {
        let mut exact = HashSet::new();
        let mut suffixes = Vec::new();
        for line in text.lines() {
            let entry = line
                .split('#')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if entry.is_empty() {
                continue;
            }
            if let Some(rest) = entry.strip_prefix("*.") {
                suffixes.push(format!(".{rest}"));
            } else if entry.starts_with('.') {
                suffixes.push(entry);
            } else {
                exact.insert(entry);
            }
        }
        Self { exact, suffixes }
    }

    /// The process-wide seed allowlist (parsed once).
    pub fn seed() -> &'static Allowlist {
        static SEED: OnceLock<Allowlist> = OnceLock::new();
        SEED.get_or_init(|| Allowlist::parse(SEED_ALLOWLIST))
    }

    /// True if `host` is allowed (exact match, or a subdomain/apex of a
    /// `*.`/`.` suffix entry). Trailing-dot and case are normalised.
    pub fn allows(&self, host: &str) -> bool {
        let h = host.trim_end_matches('.').to_ascii_lowercase();
        if h.is_empty() {
            return false;
        }
        if self.exact.contains(&h) {
            return true;
        }
        // `.github.com` matches `api.github.com` (ends_with) and the bare
        // `github.com` apex (== suffix without the leading dot).
        self.suffixes
            .iter()
            .any(|sfx| h.ends_with(sfx.as_str()) || h == sfx[1..])
    }
}

/// Log + (in deny mode) enforce an outbound request to `url` by `caller`.
/// Returns `Err` only when the mode is [`Mode::Deny`] and the host is unlisted.
pub fn guard(caller: &str, url: &str) -> Result<()> {
    let allow = Allowlist::seed();
    // Phase 1 collection: persist an egress record for later allowlist auditing
    // (best-effort; never blocks the request).
    if let Some((host, port)) = host_port_of(url) {
        record(caller, &host, port, "request", allow.allows(&host));
    }
    guard_with(caller, url, Mode::from_env(), allow)
}

/// Testable core of [`guard`] with explicit mode + allowlist.
pub fn guard_with(caller: &str, url: &str, mode: Mode, allow: &Allowlist) -> Result<()> {
    let host = host_of(url).unwrap_or_default();
    let ok = allow.allows(&host);
    tracing::debug!(target: "crabcc::netlog", caller, host = %host, allowed = ok, "egress");
    if !ok {
        match mode {
            Mode::Deny => bail!(
                "netlog: outbound host `{host}` (caller `{caller}`) is not on the crabcc \
                 allowlist — add it to crabcc-cli/src/netlog_allowlist.txt, or set \
                 CRABCC_NETLOG_DENY=0 to allow this run"
            ),
            Mode::Audit => {
                tracing::warn!(target: "crabcc::netlog", caller, host = %host, "audit: unlisted outbound host")
            }
            Mode::LogOnly => {}
        }
    }
    Ok(())
}

/// Host + port from a URL via reqwest's bundled `url` parser (handles userinfo,
/// IPv6, IDNA). Port falls back to the scheme default (443/80); 0 if unknown.
fn host_port_of(url: &str) -> Option<(String, u16)> {
    let u = reqwest::Url::parse(url).ok()?;
    let host = u.host_str()?.to_string();
    Some((host, u.port_or_known_default().unwrap_or(0)))
}

/// Just the host (used by [`guard_with`]).
fn host_of(url: &str) -> Option<String> {
    host_port_of(url).map(|(h, _)| h)
}

/// Egress-log size cap. Past this the log rotates to `netlog.jsonl.1` (one
/// backup kept), bounding disk to ~2× this.
const NETLOG_CAP_BYTES: u64 = 4 * 1024 * 1024;

/// `$CRABCC_HOME/netlog.jsonl` (or `~/.crabcc/netlog.jsonl`).
fn netlog_path() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    std::env::var_os("CRABCC_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".crabcc")))
        .map(|home| home.join("netlog.jsonl"))
}

/// Best-effort append of an egress record (issue #160 Phase 1 collection) —
/// `{ts, caller, op, host, port, ok}` JSONL. Never errors into the caller; this
/// is what grounds the allowlist in real usage (run callers under
/// `CRABCC_NETLOG_DENY=audit`, then build the allowlist from the captured log).
fn record(caller: &str, host: &str, port: u16, op: &str, ok: bool) {
    if let Some(path) = netlog_path() {
        record_to(&path, caller, host, port, op, ok, NETLOG_CAP_BYTES);
    }
}

/// Testable core of [`record`] with an explicit path + cap.
fn record_to(
    path: &std::path::Path,
    caller: &str,
    host: &str,
    port: u16,
    op: &str,
    ok: bool,
    cap: u64,
) {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Rotate before appending once the log reaches the cap.
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) >= cap {
        let _ = std::fs::rename(path, path.with_file_name("netlog.jsonl.1"));
    }
    let line = serde_json::json!({
        "ts": now_unix(),
        "caller": caller,
        "op": op,
        "host": host,
        "port": port,
        "ok": ok,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether a redirect hop to `host` should be followed. A redirect from an
/// allowed host to an unlisted one is egress to an unlisted host, so deny mode
/// blocks it; log-only/audit never block (matching [`guard`]).
fn follow_redirect(host: &str, mode: Mode, allow: &Allowlist) -> bool {
    allow.allows(host) || mode != Mode::Deny
}

/// Build an HTTP client whose redirect policy re-applies the allowlist to every
/// hop — so a 3xx from an allowed host can't smuggle egress to an unlisted one
/// past the guard (which only sees the initial URL). Pair with [`guard`] on the
/// first request: guard covers the initial host, this covers the redirect tail.
pub fn http_client(caller: &'static str) -> reqwest::Result<reqwest::Client> {
    /// Match reqwest's default loop/depth protection — a custom policy opts out
    /// of it, so we must re-impose the cap or a same-host redirect loop spins
    /// forever instead of failing with too-many-redirects.
    const MAX_REDIRECTS: usize = 10;
    let mode = Mode::from_env();
    let allow = Allowlist::seed();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error(format!(
                "netlog: too many redirects (>{MAX_REDIRECTS}) for caller `{caller}`"
            ));
        }
        let host = attempt.url().host_str().unwrap_or("").to_string();
        if follow_redirect(&host, mode, allow) {
            attempt.follow()
        } else {
            tracing::warn!(target: "crabcc::netlog", caller, host = %host, "blocked redirect to unlisted host");
            attempt.error(format!(
                "netlog: redirect to unlisted host `{host}` blocked (caller `{caller}`)"
            ))
        }
    });
    reqwest::Client::builder().redirect(policy).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn al() -> Allowlist {
        Allowlist::parse("# comment\napi.github.com\n*.example.com\n.morphllm.com\nlocalhost\n")
    }

    #[test]
    fn exact_and_case_and_trailing_dot() {
        let a = al();
        assert!(a.allows("api.github.com"));
        assert!(a.allows("API.GitHub.com")); // case-insensitive
        assert!(a.allows("localhost.")); // trailing dot normalised
        assert!(!a.allows("github.com")); // exact entry is not a wildcard
        assert!(!a.allows("evil.com"));
        assert!(!a.allows(""));
    }

    #[test]
    fn wildcard_matches_subdomain_and_apex() {
        let a = al();
        assert!(a.allows("foo.example.com")); // *.example.com
        assert!(a.allows("a.b.example.com")); // nested subdomain
        assert!(a.allows("example.com")); // bare apex
        assert!(a.allows("api.morphllm.com")); // .morphllm.com form
        assert!(a.allows("morphllm.com")); // apex
        assert!(!a.allows("notexample.com")); // suffix must be boundary
        assert!(!a.allows("example.com.evil.com"));
    }

    #[test]
    fn seed_allows_known_infra_hosts() {
        let s = Allowlist::seed();
        for h in [
            "api.morphllm.com",
            "api.github.com",
            "crates.io",
            "localhost",
        ] {
            assert!(s.allows(h), "seed should allow {h}");
        }
    }

    #[test]
    fn host_extraction() {
        assert_eq!(
            host_of("https://api.morphllm.com/v1/compact").as_deref(),
            Some("api.morphllm.com")
        );
        assert_eq!(
            host_of("http://localhost:11434/api").as_deref(),
            Some("localhost")
        );
        assert_eq!(host_of("not a url"), None);
    }

    #[test]
    fn host_port_extraction() {
        assert_eq!(
            host_port_of("https://api.morphllm.com/v1").unwrap(),
            ("api.morphllm.com".to_string(), 443)
        );
        assert_eq!(
            host_port_of("http://localhost:11434/api").unwrap(),
            ("localhost".to_string(), 11434)
        );
        assert_eq!(
            host_port_of("http://example.com/").unwrap(),
            ("example.com".to_string(), 80) // scheme default
        );
        assert!(host_port_of("not a url").is_none());
    }

    #[test]
    fn record_to_appends_jsonl_and_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("netlog.jsonl");
        record_to(
            &path,
            "morph",
            "api.morphllm.com",
            443,
            "request",
            true,
            1_000_000,
        );

        let v: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(&path).unwrap().trim()).unwrap();
        assert_eq!(v["caller"], "morph");
        assert_eq!(v["host"], "api.morphllm.com");
        assert_eq!(v["port"], 443);
        assert_eq!(v["ok"], true);
        assert_eq!(v["op"], "request");
        assert!(v["ts"].as_u64().unwrap() > 0);

        // A tiny cap forces the existing log to rotate before the next append.
        record_to(&path, "x", "evil.com", 80, "request", false, 1);
        assert!(
            path.with_file_name("netlog.jsonl.1").exists(),
            "rotated backup should exist"
        );
        assert!(std::fs::read_to_string(&path).unwrap().contains("evil.com"));
    }

    #[test]
    fn deny_blocks_unlisted_allows_listed() {
        let a = al();
        // Listed → ok in every mode.
        assert!(guard_with("t", "https://api.github.com/x", Mode::Deny, &a).is_ok());
        // Unlisted → blocked only in Deny.
        assert!(guard_with("t", "https://evil.com/x", Mode::Deny, &a).is_err());
        assert!(guard_with("t", "https://evil.com/x", Mode::LogOnly, &a).is_ok());
        assert!(guard_with("t", "https://evil.com/x", Mode::Audit, &a).is_ok());
    }

    #[test]
    fn redirect_follows_listed_blocks_unlisted_in_deny() {
        let a = al();
        assert!(follow_redirect("api.github.com", Mode::Deny, &a)); // listed → follow
        assert!(!follow_redirect("evil.com", Mode::Deny, &a)); // unlisted + deny → block
        assert!(follow_redirect("evil.com", Mode::LogOnly, &a)); // log-only never blocks
        assert!(follow_redirect("evil.com", Mode::Audit, &a)); // audit never blocks
    }

    #[test]
    fn mode_mapping() {
        assert_eq!(Mode::from_raw(None), Mode::Deny); // unset → deny (default)
        assert_eq!(Mode::from_raw(Some("1")), Mode::Deny);
        assert_eq!(Mode::from_raw(Some("0")), Mode::LogOnly);
        assert_eq!(Mode::from_raw(Some("audit")), Mode::Audit);
        assert_eq!(Mode::from_raw(Some("nonsense")), Mode::Deny);
    }
}
