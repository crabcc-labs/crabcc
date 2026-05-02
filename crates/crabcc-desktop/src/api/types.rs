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

use serde::{Deserialize, Serialize};

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

// ── SSE-specific frames ────────────────────────────────────────────────
// `/api/events` (the SSE stream) carries a slightly different schema
// than the polled `/api/activity` and `/api/agents` GETs above:
//   - activity frames wrap `events` (not `items`), have `repo` +
//     `cursor`, and the per-row count field is `results` (not `count`).
//   - agents frames carry extra fields (`runtime`, `log_bytes`, `root`)
//     that the polled HTTP shape doesn't.
// `openapi.yaml` declares the SSE response as a bare `string`, so this
// is the source of truth on the Rust side. Caught at A.3 implementation
// time by sampling live frames.

#[derive(Debug, Clone, Deserialize)]
pub struct SseActivityEvent {
    pub ts: i64,
    pub op: String,
    pub query: String,
    /// Wire field is `results`, not `count` — this is the SSE-side
    /// shape, not the `/api/activity` HTTP shape.
    pub results: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SseActivityFrame {
    pub repo: String,
    pub cursor: i64,
    pub events: Vec<SseActivityEvent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SseAgent {
    pub id: String,
    pub status: AgentStatus,
    #[serde(default)]
    pub started_ts: i64,
    pub pid: Option<u64>,
    pub runtime: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub prompt_preview: String,
    #[serde(default)]
    pub log_bytes: u64,
    pub root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SseAgentsFrame {
    pub agents: Vec<SseAgent>,
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

// ── /api/memory/recent ────────────────────────────────────────────────
// Memory-drawer feed for the Knowledge route. Wire shape captured from
// a live `crabcc serve`; openapi.yaml lists `/api/memory/recent` with
// an opaque schema, so this is the source of truth on the Rust side.

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryDrawer {
    pub id: i64,
    pub wing: String,
    pub room: Option<String>,
    pub source_id: String,
    /// Server-side trims to ~200 chars and adds an ellipsis.
    pub body_preview: String,
    /// Unix-seconds; same convention as activity / telemetry.
    pub created_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryRecentResponse {
    /// `false` if the memory backend isn't bootstrapped (no `.crabcc/memory.db`).
    pub present: bool,
    /// `id` of the highest drawer seen so far. Used by callers that
    /// want incremental fetches; we currently always GET fresh.
    #[serde(default)]
    pub cursor: i64,
    pub drawers: Vec<MemoryDrawer>,
}

/// `POST /api/memory/ingest` body. All fields optional — the server
/// accepts any subset; an empty body just no-ops with `stats.ok = 0`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryIngestRequest {
    /// Free-form note body. Hashed server-side into `text:HASH` ids.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// URLs to fetch + ingest. Server-side runs `crabcc-fetch` against
    /// each, with the same SSRF guards the CLI applies.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
    /// Tag for the resulting drawer's `wing` (e.g. "desktop:ingest").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryIngestStats {
    pub ok: u32,
    pub failed: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryIngestEntry {
    pub id: String,
    pub kind: String,
    pub bytes: u64,
    pub drawer_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryIngestResponse {
    pub ingested: Vec<MemoryIngestEntry>,
    /// Per-input error strings; matches the request order one-to-one.
    pub errors: Vec<String>,
    pub stats: MemoryIngestStats,
}

/// `POST /api/agents/launch` body.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentLaunchRequest {
    pub prompt: String,
    /// Optional override; server uses its configured default when None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Live response shape (verified via direct probes) — slightly wider
/// than the openapi `AgentLaunchResponse` schema, which only declares
/// `id` + `pid`. Server actually emits `ok`, `prompt_chars`,
/// `timeout_secs` too. All fields kept `Option<…>` so future server
/// drift in nullability doesn't crash decode.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentLaunchResponse {
    pub id: Option<String>,
    #[serde(default)]
    pub ok: bool,
    pub pid: Option<u64>,
    #[serde(default)]
    pub prompt_chars: u64,
    #[serde(default)]
    pub timeout_secs: u64,
}

// ── /api/seed-graph (relations graph) ──────────────────────────────────
// Legacy endpoint per `openapi.yaml`'s tag — the server emits free-form
// JSON, but the React `Graph.tsx` consumer locks the shape to the
// fields below. Mirroring that on the Rust side until the call-graph
// viewer is migrated fully (server-side TODO).

#[derive(Debug, Clone, Deserialize)]
pub struct GraphNode {
    pub id: String,
    /// BFS depth from the seed set. The web Graph.tsx ignores this
    /// today, but the layout in `routes::graph` colours deeper nodes
    /// dimmer so the entry points pop.
    #[serde(default)]
    pub depth: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GraphEdge {
    pub src: String,
    pub dst: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GraphSnapshot {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Seed identifiers — the BFS roots. Surfaced by the server so the
    /// viewer can mark them visually; we currently ignore the field
    /// but accept it so decode doesn't fail.
    #[serde(default)]
    pub seeds: Vec<String>,
}
