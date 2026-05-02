// Wire types — Rust analogues of `crates/crabcc-viz/openapi.yaml` schemas.
// The TS side regenerates `web/src/api.gen.ts` from that YAML at build
// time; the Rust side is hand-maintained and intentionally tracks only
// what `crabcc-desktop` consumes. When the YAML drifts, this file is the
// source of truth on the Rust side until we adopt a generator.
//
// Conventions:
//   - Field names mirror the JSON keys exactly (snake_case throughout).
//   - `Option<T>` for every YAML `nullable: true` field.
//   - Numeric `unix seconds` timestamps stay `i64` to match the server.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct HealthResponse {
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BootstrapIndex {
    pub present: Option<bool>,
    pub files: Option<u64>,
    pub symbols: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Bootstrap {
    pub repo: String,
    pub root: String,
    pub version: String,
    pub index: Option<BootstrapIndex>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActivityHit {
    pub ts: i64,
    pub op: String,
    pub query: String,
    pub count: u64,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActivityResponse {
    pub items: Vec<ActivityHit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Running,
    Exited,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentSummary {
    pub id: String,
    pub status: AgentStatus,
    pub pid: Option<u64>,
    pub prompt_preview: Option<String>,
    pub model: Option<String>,
    pub started_ts: Option<i64>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentsResponse {
    pub agents: Vec<AgentSummary>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentLog {
    pub body: String,
    pub cursor: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentProfileEntry {
    pub id: String,
    /// Trailing underscore mirrors the openapi field — `crate` is a
    /// reserved keyword in Rust, same as in TS.
    pub crate_: Option<String>,
    pub description: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentProfilesResponse {
    pub dir: String,
    pub profiles: Vec<AgentProfileEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentKillRow {
    pub run_id: String,
    pub reason: String,
    pub pid: Option<u64>,
    pub detail: Option<String>,
    pub killed_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentKillsResponse {
    pub db: String,
    pub rows: Vec<AgentKillRow>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentModelEntry {
    pub file: String,
    pub provider: String,
    pub name: String,
    pub params: Option<String>,
    pub context: Option<u64>,
    pub docs_first: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentModelsResponse {
    pub dir: String,
    pub models: Vec<AgentModelEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OllamaKey {
    pub present: bool,
    pub path: String,
    pub mode: Option<String>,
    pub mtime_secs: Option<i64>,
    pub size_bytes: Option<u64>,
    pub key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    Redis,
    HttpJsonApi,
    OtlpGrpc,
    OtlpHttp,
    Ollama,
    /// Added server-side in issue #204 (Phase 0 — service-discovery
    /// MCP variant). The `openapi.yaml` enum currently lags; the
    /// catch-all `Unknown` below absorbs any further drift until the
    /// YAML catches up.
    Mcp,
    Generic,
    /// Catch-all so a freshly-added server-side variant doesn't crash
    /// the desktop client at JSON-decode time. UI should render these
    /// with a neutral "unknown service kind" pill until the type is
    /// promoted here.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceStatus {
    pub name: String,
    pub kind: ServiceKind,
    pub url: String,
    pub source: String,
    pub host: String,
    pub port: u16,
    pub reachable: bool,
    pub latency_ms: u64,
    pub error: Option<String>,
    pub probed_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveryReport {
    pub services: Vec<ServiceStatus>,
    pub compose_mode: bool,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelemetryEvent {
    pub ts: i64,
    pub level: LogLevel,
    pub target: String,
    /// `additionalProperties: true` on the YAML side — server emits
    /// whatever structured fields it has. Kept as a raw JSON value so
    /// the desktop UI can introspect without schema churn.
    pub fields: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelemetrySource {
    pub path: String,
    pub lines_read: u64,
    pub bytes: u64,
    pub exists: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelemetrySnapshot {
    pub cursor: u64,
    pub events: Vec<TelemetryEvent>,
    pub source: TelemetrySource,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OtlpHealth {
    pub reachable: bool,
    pub endpoint: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReindexReport {
    pub root: String,
    pub elapsed_ms: u64,
    /// Mixed-type values (string + integer) per openapi `oneOf`.
    pub stats: serde_json::Value,
    pub logs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RandomQueryResponse {
    pub op: String,
    pub symbol: String,
}
