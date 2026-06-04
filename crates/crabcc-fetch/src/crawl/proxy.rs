//! Opt-in proxy rotation. **Disabled by default** — a crawl only routes
//! through proxies when the caller explicitly supplies a pool.
//!
//! [`ProxySource`] abstracts *where* proxies come from and *how* to parse
//! the published list; the engine performs the actual fetch so the trait
//! stays sync and unit-testable. [`ProxiflySource`] wires the
//! [proxifly free-proxy-list][proxifly] (served via jsDelivr,
//! revalidated ~every 5 min). Its GPL-3.0 license covers the repo's
//! code; we only *fetch* the published data at runtime (no
//! redistribution), so it imposes no obligation on crabcc.
//!
//! Free public proxies are flaky and can MITM — keep treating every
//! fetched response as untrusted (the SSRF guards and parser already
//! assume that), and never send credentials through them. A paid pool
//! slots in behind the same [`ProxySource`] trait later.
//!
//! [proxifly]: https://github.com/proxifly/free-proxy-list

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;
use std::time::Duration;

/// Consecutive failures a proxy may rack up before it's evicted from
/// rotation. Free proxies die constantly, so this is deliberately small.
pub const DEFAULT_MAX_FAILURES: u32 = 3;

/// Lightweight endpoint a health probe GETs through a proxy — returns an
/// empty `204`, so it's cheap and unambiguous.
pub const HEALTH_CHECK_URL: &str = "http://www.gstatic.com/generate_204";

/// A proxy endpoint in reqwest's `scheme://host:port` form.
pub type ProxyUrl = String;

/// Proxy wire protocols proxifly publishes a list per.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Http,
    Https,
    Socks4,
    Socks5,
}

impl Protocol {
    /// The path segment proxifly uses (`/protocols/<seg>/data.txt`).
    pub fn segment(self) -> &'static str {
        match self {
            Protocol::Http => "http",
            Protocol::Https => "https",
            Protocol::Socks4 => "socks4",
            Protocol::Socks5 => "socks5",
        }
    }

    /// The URL scheme to prefix bare `host:port` entries with.
    pub fn scheme(self) -> &'static str {
        self.segment()
    }
}

/// Where a [`ProxyPool`] sources candidate proxies. The endpoint is
/// fetched by the engine; `parse` turns the body into proxy URLs.
pub trait ProxySource: Send + Sync {
    /// HTTP(S) URL returning the raw proxy list.
    fn endpoint(&self) -> String;
    /// Parse a fetched list body into `scheme://host:port` proxy URLs.
    fn parse(&self, body: &str) -> Vec<ProxyUrl>;
    /// Short label for logs.
    fn label(&self) -> &'static str;
}

/// The proxifly free-proxy-list, one protocol per source.
pub struct ProxiflySource {
    pub protocol: Protocol,
}

impl ProxiflySource {
    pub fn new(protocol: Protocol) -> Self {
        Self { protocol }
    }
}

impl ProxySource for ProxiflySource {
    fn endpoint(&self) -> String {
        format!(
            "https://cdn.jsdelivr.net/gh/proxifly/free-proxy-list@main/proxies/protocols/{}/data.txt",
            self.protocol.segment()
        )
    }

    fn parse(&self, body: &str) -> Vec<ProxyUrl> {
        let scheme = self.protocol.scheme();
        body.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| {
                if l.contains("://") {
                    l.to_string()
                } else {
                    format!("{scheme}://{l}")
                }
            })
            .collect()
    }

    fn label(&self) -> &'static str {
        "proxifly"
    }
}

/// Mutable pool state: the live rotation plus per-proxy consecutive-
/// failure counts, kept together under one lock so health updates and
/// `pick` never race.
struct State {
    live: Vec<ProxyUrl>,
    failures: HashMap<ProxyUrl, u32>,
}

/// A rotating, self-healing pool of proxy URLs. Round-robins via an atomic
/// cursor; health-checking evicts dead entries two ways (free proxies
/// churn fast):
///
/// - **passive** — [`report_failure`](Self::report_failure) /
///   [`report_success`](Self::report_success) from the fetch path drop a
///   proxy after [`DEFAULT_MAX_FAILURES`] consecutive failures;
/// - **active** — [`prune_dead`](Self::prune_dead) probes every live proxy
///   and evicts the ones that don't answer.
///
/// State is behind one `RwLock`, so `pick` (read) and the health mutators
/// (write) are safe to call concurrently from worker tasks. `pick` returns
/// an owned `String` since the lock can't outlive the call.
pub struct ProxyPool {
    state: RwLock<State>,
    cursor: AtomicUsize,
    max_failures: u32,
}

impl ProxyPool {
    /// Fetch `source`'s list with `client` and build a pool from it.
    pub async fn load(client: &reqwest::Client, source: &dyn ProxySource) -> anyhow::Result<Self> {
        let body = client.get(source.endpoint()).send().await?.text().await?;
        let proxies = source.parse(&body);
        tracing::info!(
            target: "crabcc_fetch",
            source = source.label(),
            count = proxies.len(),
            "loaded proxy pool",
        );
        Ok(Self::from_list(proxies))
    }

    pub fn from_list(proxies: Vec<ProxyUrl>) -> Self {
        Self {
            state: RwLock::new(State {
                live: proxies,
                failures: HashMap::new(),
            }),
            cursor: AtomicUsize::new(0),
            max_failures: DEFAULT_MAX_FAILURES,
        }
    }

    /// Override the consecutive-failure eviction threshold.
    pub fn with_max_failures(mut self, max_failures: u32) -> Self {
        self.max_failures = max_failures.max(1);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.state.read().unwrap().live.is_empty()
    }

    pub fn len(&self) -> usize {
        self.state.read().unwrap().live.len()
    }

    /// Next proxy in round-robin order, or `None` when the pool is empty.
    pub fn pick(&self) -> Option<ProxyUrl> {
        let state = self.state.read().unwrap();
        if state.live.is_empty() {
            return None;
        }
        let i = self.cursor.fetch_add(1, Ordering::Relaxed) % state.live.len();
        Some(state.live[i].clone())
    }

    /// Record a successful fetch through `proxy`, clearing its failure
    /// streak so a transient blip doesn't accumulate toward eviction.
    pub fn report_success(&self, proxy: &str) {
        self.state.write().unwrap().failures.remove(proxy);
    }

    /// Record a failed fetch through `proxy`. Returns `true` if this
    /// tripped the threshold and the proxy was evicted from rotation.
    pub fn report_failure(&self, proxy: &str) -> bool {
        let mut state = self.state.write().unwrap();
        let n = state.failures.entry(proxy.to_string()).or_insert(0);
        *n += 1;
        if *n >= self.max_failures {
            remove(&mut state, proxy);
            true
        } else {
            false
        }
    }

    /// Drop a proxy from rotation immediately, regardless of failure count.
    pub fn evict(&self, proxy: &str) {
        remove(&mut self.state.write().unwrap(), proxy);
    }

    /// Active health check: probe every live proxy and evict the dead
    /// ones. Returns how many were dropped. Sequential (an occasional
    /// sweep, not a hot path) so it needs no extra concurrency deps.
    pub async fn prune_dead(&self, test_url: &str, timeout: Duration) -> usize {
        let live: Vec<ProxyUrl> = self.state.read().unwrap().live.clone();
        let mut evicted = 0;
        for proxy in live {
            if !probe(&proxy, test_url, timeout).await {
                self.evict(&proxy);
                evicted += 1;
            }
        }
        if evicted > 0 {
            tracing::info!(target: "crabcc_fetch", evicted, "proxy health check evicted dead proxies");
        }
        evicted
    }

    /// Active health check against the default [`HEALTH_CHECK_URL`].
    pub async fn health_check(&self, timeout: Duration) -> usize {
        self.prune_dead(HEALTH_CHECK_URL, timeout).await
    }
}

/// Remove a proxy from both the live list and the failure map.
fn remove(state: &mut State, proxy: &str) {
    state.live.retain(|p| p != proxy);
    state.failures.remove(proxy);
}

/// Probe a single proxy: GET `test_url` through it and treat a 2xx as
/// alive. Any build/connect/timeout error is "dead". Network I/O, so this
/// is exercised by integration runs, not the unit tests.
async fn probe(proxy: &str, test_url: &str, timeout: Duration) -> bool {
    let Ok(reqwest_proxy) = reqwest::Proxy::all(proxy) else {
        return false;
    };
    let Ok(client) = reqwest::Client::builder()
        .proxy(reqwest_proxy)
        .timeout(timeout)
        .build()
    else {
        return false;
    };
    matches!(client.get(test_url).send().await, Ok(r) if r.status().is_success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_and_schemed_lines() {
        let src = ProxiflySource::new(Protocol::Socks5);
        let body = "# comment\n1.2.3.4:1080\n\nsocks5://5.6.7.8:1080\n   9.9.9.9:1   \n";
        let got = src.parse(body);
        assert_eq!(
            got,
            vec![
                "socks5://1.2.3.4:1080".to_string(),
                "socks5://5.6.7.8:1080".to_string(),
                "socks5://9.9.9.9:1".to_string(),
            ]
        );
    }

    #[test]
    fn endpoint_includes_protocol_segment() {
        let ep = ProxiflySource::new(Protocol::Http).endpoint();
        assert!(ep.ends_with("/protocols/http/data.txt"), "{ep}");
    }

    #[test]
    fn pool_round_robins_and_handles_empty() {
        let pool = ProxyPool::from_list(vec!["http://a:1".into(), "http://b:2".into()]);
        assert_eq!(pool.pick().as_deref(), Some("http://a:1"));
        assert_eq!(pool.pick().as_deref(), Some("http://b:2"));
        assert_eq!(pool.pick().as_deref(), Some("http://a:1"));
        assert!(ProxyPool::from_list(vec![]).pick().is_none());
    }

    #[test]
    fn evict_removes_dead_proxy() {
        let pool = ProxyPool::from_list(vec!["http://a:1".into(), "http://b:2".into()]);
        pool.evict("http://a:1");
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.pick().as_deref(), Some("http://b:2"));
        pool.evict("http://b:2");
        assert!(pool.is_empty());
        assert!(pool.pick().is_none());
    }

    #[test]
    fn consecutive_failures_evict_at_threshold() {
        let pool = ProxyPool::from_list(vec!["http://a:1".into(), "http://b:2".into()])
            .with_max_failures(3);
        assert!(!pool.report_failure("http://a:1")); // 1
        assert!(!pool.report_failure("http://a:1")); // 2
        assert!(pool.report_failure("http://a:1")); // 3 → evicted
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.pick().as_deref(), Some("http://b:2"));
    }

    #[test]
    fn success_resets_the_failure_streak() {
        let pool = ProxyPool::from_list(vec!["http://a:1".into()]).with_max_failures(2);
        assert!(!pool.report_failure("http://a:1")); // 1
        pool.report_success("http://a:1"); // streak cleared
        assert!(!pool.report_failure("http://a:1")); // 1 again, not 2
        assert_eq!(pool.len(), 1);
        assert!(pool.report_failure("http://a:1")); // 2 → evicted
        assert!(pool.is_empty());
    }

    #[test]
    fn max_failures_is_floored_at_one() {
        // A zero threshold would evict on the first failure rather than
        // never — clamp to 1 so the pool stays sane.
        let pool = ProxyPool::from_list(vec!["http://a:1".into()]).with_max_failures(0);
        assert!(pool.report_failure("http://a:1"));
        assert!(pool.is_empty());
    }
}
