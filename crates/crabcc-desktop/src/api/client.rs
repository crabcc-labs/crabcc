// Blocking HTTP client over `crabcc serve`'s `/api/*` surface.
// Mirrors `crates/crabcc-viz/web/src/api.ts` shape-for-shape so a UI
// engineer reading the JS side immediately recognises this.
//
// Threading: the client is `Send + Sync` (reqwest::blocking::Client is),
// so a single `Arc<Client>` lives on AppState and individual route
// handlers call into it from a `cx.background_executor().spawn(...)`
// closure. SSE / streaming is intentionally out of scope here — that's
// A.3.

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};

use super::types::{
    ActivityResponse, AgentKillResponse, AgentKillsResponse, AgentLaunchRequest,
    AgentLaunchResponse, AgentLog, AgentModelsResponse, AgentProfilesResponse, AgentsResponse,
    Bootstrap, DiscoveryReport, GraphSnapshot, HealthResponse, MemoryGraphResponse,
    MemoryIngestRequest, MemoryIngestResponse, MemoryRecentResponse, OllamaKey, OtlpHealth,
    RandomQueryResponse, ReindexReport, TelemetrySnapshot,
};

/// Default loopback origin for `crabcc serve`. Override via
/// `Client::with_base_url` for tests / non-default ports.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:7878";

#[derive(Debug, Clone)]
pub struct Client {
    base_url: String,
    http: reqwest::blocking::Client,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    pub fn with_base_url(base: impl Into<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            // Loopback-only — short timeout is correct, surfaces a dead
            // server immediately instead of hanging the UI thread.
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("reqwest blocking client builds with default tls");
        Self {
            base_url: base.into(),
            http,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = self.url(path);
        let resp = self
            .http
            .get(&url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("GET {url} → {status}: {body}");
        }
        resp.json::<T>()
            .with_context(|| format!("decode JSON from {url}"))
    }

    fn post_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = self.url(path);
        let resp = self
            .http
            .post(&url)
            .send()
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("POST {url} → {status}: {body}");
        }
        resp.json::<T>()
            .with_context(|| format!("decode JSON from {url}"))
    }

    fn post_json_body<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let url = self.url(path);
        let resp = self
            .http
            .post(&url)
            .json(body)
            .send()
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("POST {url} → {status}: {body}");
        }
        resp.json::<T>()
            .with_context(|| format!("decode JSON from {url}"))
    }

    // ── endpoint methods ────────────────────────────────────────────────
    // Order mirrors `web/src/api.ts` so a TS↔Rust diff is mechanical.

    pub fn health(&self) -> Result<HealthResponse> {
        self.get_json("/api/health")
    }

    pub fn bootstrap(&self) -> Result<Bootstrap> {
        self.get_json("/api/bootstrap")
    }

    pub fn activity(&self, since_ts: Option<i64>, limit: u32) -> Result<ActivityResponse> {
        let since = since_ts.unwrap_or(0);
        self.get_json(&format!("/api/activity?since={since}&limit={limit}"))
    }

    pub fn agents(&self) -> Result<AgentsResponse> {
        self.get_json("/api/agents")
    }

    pub fn agent_log(&self, id: &str, since: u64) -> Result<AgentLog> {
        self.get_json(&format!("/api/agents/{id}/log?since={since}"))
    }

    pub fn agent_profiles(&self) -> Result<AgentProfilesResponse> {
        self.get_json("/api/agent-profiles")
    }

    pub fn agent_kills(&self) -> Result<AgentKillsResponse> {
        self.get_json("/api/agent-kills")
    }

    pub fn agent_models(&self) -> Result<AgentModelsResponse> {
        self.get_json("/api/agent-models")
    }

    pub fn ollama_key(&self) -> Result<OllamaKey> {
        self.get_json("/api/ollama-key")
    }

    pub fn services(&self) -> Result<DiscoveryReport> {
        self.get_json("/api/services")
    }

    pub fn telemetry(&self, since_ts: Option<i64>, limit: u32) -> Result<TelemetrySnapshot> {
        let since = since_ts.unwrap_or(0);
        self.get_json(&format!("/api/telemetry?since={since}&limit={limit}"))
    }

    pub fn otlp_health(&self) -> Result<OtlpHealth> {
        self.get_json("/api/telemetry/otlp-health")
    }

    pub fn reindex(&self) -> Result<ReindexReport> {
        self.post_json("/api/reindex")
    }

    pub fn random_query(&self) -> Result<RandomQueryResponse> {
        self.post_json("/api/random-query")
    }

    /// Legacy seed-graph endpoint — see `crates/crabcc-viz/web/src/components/Graph.tsx`
    /// for the wire shape contract until the call-graph viewer migrates.
    pub fn seed_graph(&self) -> Result<GraphSnapshot> {
        self.get_json("/api/seed-graph")
    }

    /// Recent memory drawers (most-recently-created first). Backs the
    /// Knowledge route. Server caps the body preview to ~200 chars.
    pub fn memory_recent(&self) -> Result<MemoryRecentResponse> {
        self.get_json("/api/memory/recent?limit=50")
    }

    /// Drawer cross-reference graph for the K-Graph route (#317).
    /// Server-side scans drawer bodies for `web:<hash>`, `text:<hash>`,
    /// `doc:<n>`, and Obsidian-style `[[Title]]` references and
    /// returns nodes + resolved edges. `limit=200` matches the
    /// server's default; bump for power-user repos with deep memory.
    pub fn memory_graph(&self) -> Result<MemoryGraphResponse> {
        self.get_json("/api/memory/graph?limit=500")
    }

    /// Submit a memory drawer (free-text and/or URLs). Returns the
    /// per-input ingest record + aggregate stats. The Knowledge route
    /// surfaces the new drawer immediately by re-fetching `memory_recent`
    /// without waiting for the periodic poll.
    pub fn memory_ingest(&self, req: &MemoryIngestRequest) -> Result<MemoryIngestResponse> {
        self.post_json_body("/api/memory/ingest", req)
    }

    /// Spawn a one-off agent. Server returns its pid + the configured
    /// timeout; the agent's stdout / stderr lands in the existing
    /// telemetry / activity feeds, so the desktop UI doesn't need a
    /// streaming response here — the dashboard's existing pumps surface
    /// progress.
    pub fn agent_launch(&self, req: &AgentLaunchRequest) -> Result<AgentLaunchResponse> {
        self.post_json_body("/api/agents/launch", req)
    }

    /// SIGKILL a running agent. `signaled = true` means the signal was
    /// delivered; `signaled = false` + `note: "process already exited"`
    /// is a benign idempotent outcome (the row is exited regardless).
    /// Returns 400 from the server on an unknown agent id.
    pub fn agent_kill(&self, id: &str) -> Result<AgentKillResponse> {
        // Server allows arbitrary id strings; the path encoder doesn't
        // need to escape `[a-z0-9-]+` ids in practice.
        self.post_json(&format!("/api/agents/{id}/kill"))
    }
}
