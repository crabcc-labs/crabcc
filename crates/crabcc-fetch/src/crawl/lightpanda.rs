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
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::cdp::Cdp;
use super::fetcher::{FetchedPage, HttpFetcher};
use crate::{clean_html, is_reddit_url, url_host, FetchOpts, FetchResult, Transport};

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

/// A managed Lightpanda browser: a spawned `lightpanda serve` (or an
/// attached external endpoint) plus its CDP WebSocket URL. The spawned
/// child is killed on drop.
pub struct LightpandaProcess {
    // `None` when attached to an external endpoint we don't own. Held in a
    // `Mutex` (not a bare `Child`) so the surrounding `Fetcher` is `Sync` —
    // the crawler shares it across worker tasks via `Arc`.
    child: Mutex<Option<Child>>,
    ws_url: String,
}

impl LightpandaProcess {
    /// Start (or attach to) a Lightpanda browser per `cfg`. Synchronous —
    /// it runs at fetcher construction, before the async crawl loop.
    pub fn start(cfg: &LightpandaConfig) -> Result<Self> {
        match &cfg.source {
            LightpandaSource::External(url) => Ok(Self {
                child: Mutex::new(None),
                ws_url: normalize_ws(url),
            }),
            LightpandaSource::Spawn(bin) => {
                // Reserve the profile dir even though `lightpanda serve`
                // exposes no user-data-dir flag yet (see module docs);
                // persistence rides on $CRABCC_LIGHTPANDA_ARGS until it does.
                let _ = std::fs::create_dir_all(&cfg.profile_dir);
                let port = free_port()?;
                let mut child = Command::new(bin)
                    .arg("serve")
                    .arg("--host")
                    .arg("127.0.0.1")
                    .arg("--port")
                    .arg(port.to_string())
                    .args(extra_args())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .with_context(|| format!("spawn lightpanda: {}", bin.display()))?;
                if let Err(e) = wait_ready("127.0.0.1", port, Duration::from_secs(10)) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e);
                }
                Ok(Self {
                    child: Mutex::new(Some(child)),
                    ws_url: format!("ws://127.0.0.1:{port}"),
                })
            }
        }
    }

    pub fn ws_url(&self) -> &str {
        &self.ws_url
    }
}

impl Drop for LightpandaProcess {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

/// Lightpanda CDP transport. Renders each page in its own browser context
/// and tab, returning the DOM as markdown — degrading to the native HTTP
/// transport on any browser-side failure so a page is never dropped.
pub struct LightpandaFetcher {
    browser: LightpandaProcess,
    http: HttpFetcher,
    nav_timeout: Duration,
}

impl LightpandaFetcher {
    /// Spawn/attach the browser and build the HTTP fallback. Synchronous,
    /// matching [`Fetcher::auto`](super::Fetcher::auto)'s construction.
    pub fn start(cfg: LightpandaConfig, opts: &FetchOpts, proxy: Option<&str>) -> Result<Self> {
        Ok(Self {
            browser: LightpandaProcess::start(&cfg)?,
            http: HttpFetcher::new(opts, proxy)?,
            nav_timeout: opts.per_url_timeout,
        })
    }

    pub fn ws_url(&self) -> &str {
        self.browser.ws_url()
    }

    /// Drive one page over CDP: fresh context + tab, navigate, read the
    /// rendered DOM + final URL, tear down. Any step failing returns `Err`
    /// so the caller degrades to HTTP. Returns `(html, final_url)`.
    async fn render(&self, url: &str) -> Result<(String, String)> {
        let mut cdp = Cdp::connect(self.browser.ws_url()).await?;
        let context = cdp.create_browser_context().await?;
        let rendered = async {
            let target = cdp.create_target(url, Some(&context)).await?;
            let session = cdp.attach(&target).await?;
            cdp.navigate(&session, url, self.nav_timeout).await?;
            let html = cdp.outer_html(&session).await?;
            // Resolve links against the post-redirect URL, like the HTTP
            // path's `final_url`; fall back to the request URL.
            let final_url = cdp
                .current_url(&session)
                .await
                .unwrap_or_else(|_| url.to_string());
            let _ = cdp.close_target(&target).await;
            Ok::<_, anyhow::Error>((html, final_url))
        }
        .await;
        let _ = cdp.dispose_browser_context(&context).await;
        rendered
    }

    pub(crate) async fn fetch(&self, url: &str) -> FetchedPage {
        // Reddit is a JSON API with no crawlable DOM — defer to the HTTP
        // path that special-cases it.
        if is_reddit_url(url) {
            return self.http.fetch(url).await;
        }
        match self.render(url).await {
            Ok((html, final_url)) => {
                let host = url_host(&final_url).unwrap_or_default();
                let (title, markdown) = clean_html(host, &html);
                FetchedPage {
                    result: FetchResult {
                        url: url.into(),
                        // CDP doesn't surface an HTTP status without the
                        // Network domain; a returned DOM means the
                        // navigation succeeded.
                        status: 200,
                        title,
                        content_markdown: Some(markdown),
                        via: Transport::Chrome,
                        error: None,
                    },
                    raw_html: Some(html),
                    final_url,
                }
            }
            Err(e) => {
                tracing::debug!(
                    target: "crabcc_fetch",
                    url,
                    error = %e,
                    "lightpanda render failed; falling back to HTTP",
                );
                self.http.fetch(url).await
            }
        }
    }
}

/// Reserve an ephemeral port by binding then immediately releasing it.
/// (Small TOCTOU window before lightpanda binds — fine for local use.)
fn free_port() -> Result<u16> {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").context("reserve a port for lightpanda")?;
    Ok(listener.local_addr()?.port())
}

/// Block until `host:port` accepts a TCP connection, or time out.
fn wait_ready(host: &str, port: u16, timeout: Duration) -> Result<()> {
    let addr: std::net::SocketAddr = format!("{host}:{port}")
        .parse()
        .context("parse lightpanda address")?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!("lightpanda did not start listening on {addr} within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Extra `lightpanda serve` args from `$CRABCC_LIGHTPANDA_ARGS`
/// (whitespace-separated) — the escape hatch for flags like a
/// profile/user-data-dir once the browser exposes one.
fn extra_args() -> Vec<String> {
    non_empty_env("CRABCC_LIGHTPANDA_ARGS")
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default()
}

/// Normalize an external endpoint to a `ws(s)://` URL. Accepts `ws://`,
/// `wss://`, `http(s)://`, or a bare `host:port`.
fn normalize_ws(url: &str) -> String {
    if url.starts_with("ws://") || url.starts_with("wss://") {
        url.to_string()
    } else if let Some(rest) = url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if let Some(rest) = url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else {
        format!("ws://{url}")
    }
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

    #[test]
    fn normalize_ws_handles_each_scheme() {
        assert_eq!(normalize_ws("ws://h:9222"), "ws://h:9222");
        assert_eq!(normalize_ws("wss://h:9222"), "wss://h:9222");
        assert_eq!(normalize_ws("http://h:9222"), "ws://h:9222");
        assert_eq!(normalize_ws("https://h:9222"), "wss://h:9222");
        assert_eq!(normalize_ws("127.0.0.1:9222"), "ws://127.0.0.1:9222");
    }
}
