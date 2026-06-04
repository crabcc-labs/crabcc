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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;

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

/// A rotating pool of proxy URLs. Round-robins via an atomic cursor;
/// dead entries are evicted by [`ProxyPool::evict`] as health-checking
/// finds them (free proxies churn fast).
///
/// The list is behind an `RwLock` so [`pick`](Self::pick) (read) and
/// [`evict`](Self::evict) (write) are safe to call concurrently from the
/// worker tasks. `pick` therefore returns an owned `String` rather than a
/// borrow into the list, since the lock can't outlive the call.
pub struct ProxyPool {
    proxies: RwLock<Vec<ProxyUrl>>,
    cursor: AtomicUsize,
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
            proxies: RwLock::new(proxies),
            cursor: AtomicUsize::new(0),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.proxies.read().unwrap().is_empty()
    }

    pub fn len(&self) -> usize {
        self.proxies.read().unwrap().len()
    }

    /// Next proxy in round-robin order, or `None` when the pool is empty.
    pub fn pick(&self) -> Option<ProxyUrl> {
        let proxies = self.proxies.read().unwrap();
        if proxies.is_empty() {
            return None;
        }
        let i = self.cursor.fetch_add(1, Ordering::Relaxed) % proxies.len();
        Some(proxies[i].clone())
    }

    /// Drop a dead proxy from rotation. Safe to call while other tasks are
    /// `pick`ing — the `RwLock` serialises the mutation.
    pub fn evict(&self, proxy: &str) {
        self.proxies.write().unwrap().retain(|p| p != proxy);
    }
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
}
