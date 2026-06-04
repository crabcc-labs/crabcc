//! Lightpanda transport — process lifecycle, profile, and endpoint
//! resolution. Behind the `crawl-lightpanda` feature.
//!
//! Lightpanda is a headless, CDP-driven browser that returns the
//! *rendered DOM* (so JS-built pages work) with no screenshots or vision
//! models. Default posture is **spawn-and-manage-locally**: crabcc starts
//! `lightpanda serve`, reuses that one browser for the whole crawl, and
//! kills it on drop. Set `$CRABCC_LIGHTPANDA_URL` to attach to an
//! already-running browser (e.g. a shared `cloakserve`) instead.
//!
//! The user **profile + cookie jar live under
//! `$CRABCC_HOME/lightpanda/profile`** and are reused across crawls, so
//! logins/cookies persist between runs.
//!
//! This commit lands the lifecycle/detection half; the CDP rendered-DOM
//! fetch (driving the browser over its WebSocket) is the next layer. Until
//! then [`super::Fetcher::auto`] still resolves to HTTP even when
//! Lightpanda is detected — see its TODO.

use std::path::{Path, PathBuf};

/// How the crawler obtains a Lightpanda CDP endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LightpandaSource {
    /// Attach to an already-running browser at this CDP URL
    /// (`$CRABCC_LIGHTPANDA_URL`).
    External(String),
    /// Spawn and manage `lightpanda serve` locally using this binary.
    Spawn(PathBuf),
}

/// Resolved Lightpanda configuration for a crawl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightpandaConfig {
    /// Persistent user profile + cookie jar, reused across crawls.
    pub profile_dir: PathBuf,
    /// Where to get the browser from.
    pub source: LightpandaSource,
}

impl LightpandaConfig {
    /// Resolve from the environment, or `None` when Lightpanda isn't
    /// available — no `$CRABCC_LIGHTPANDA_URL` and no binary found — in
    /// which case the caller falls back to the HTTP transport.
    pub fn from_env() -> Option<Self> {
        let profile_dir = profile_dir_in(&crabcc_home());
        let url = non_empty_env("CRABCC_LIGHTPANDA_URL");
        let bin = resolve_binary(
            non_empty_env("CRABCC_LIGHTPANDA_BIN").map(PathBuf::from),
            std::env::var_os("PATH"),
        );
        resolve_source(url, bin).map(|source| Self {
            profile_dir,
            source,
        })
    }
}

/// `$CRABCC_HOME`, else `~/.crabcc` — mirrors the convention the memory
/// Palace uses for its per-repo stores.
fn crabcc_home() -> PathBuf {
    if let Some(h) = non_empty_env("CRABCC_HOME") {
        return PathBuf::from(h);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".crabcc")
}

/// The reused profile/cookie directory under a given crabcc home.
fn profile_dir_in(home: &Path) -> PathBuf {
    home.join("lightpanda").join("profile")
}

/// An external CDP URL wins; otherwise spawn-and-manage a found binary.
fn resolve_source(url: Option<String>, bin: Option<PathBuf>) -> Option<LightpandaSource> {
    if let Some(url) = url {
        return Some(LightpandaSource::External(url));
    }
    bin.map(LightpandaSource::Spawn)
}

/// `$CRABCC_LIGHTPANDA_BIN` (if it exists), else `lightpanda` on `$PATH`.
fn resolve_binary(explicit: Option<PathBuf>, path: Option<std::ffi::OsString>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return p.exists().then_some(p);
    }
    let path = path?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("lightpanda"))
        .find(|p| p.exists())
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_dir_lives_under_crabcc_home() {
        let dir = profile_dir_in(Path::new("/home/u/.crabcc"));
        assert_eq!(dir, PathBuf::from("/home/u/.crabcc/lightpanda/profile"));
    }

    #[test]
    fn external_url_takes_precedence_over_binary() {
        let src = resolve_source(
            Some("ws://127.0.0.1:9222".into()),
            Some(PathBuf::from("/usr/bin/lightpanda")),
        );
        assert_eq!(
            src,
            Some(LightpandaSource::External("ws://127.0.0.1:9222".into()))
        );
    }

    #[test]
    fn no_url_no_binary_means_unavailable() {
        assert_eq!(resolve_source(None, None), None);
    }

    #[test]
    fn spawn_when_only_binary_present() {
        let src = resolve_source(None, Some(PathBuf::from("/opt/lightpanda")));
        assert_eq!(
            src,
            Some(LightpandaSource::Spawn(PathBuf::from("/opt/lightpanda")))
        );
    }

    #[test]
    fn missing_explicit_binary_is_not_used() {
        // A non-existent explicit path resolves to None rather than being
        // blindly trusted.
        assert_eq!(
            resolve_binary(Some(PathBuf::from("/nope/lightpanda-xyz")), None),
            None
        );
    }
}
