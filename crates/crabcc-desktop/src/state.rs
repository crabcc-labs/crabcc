//! `AppState` — the shared model the dashboard view observes.
//!
//! Holds the latest snapshot of bootstrap, agents, services, telemetry,
//! and ring buffers for recent activity + telemetry events. Four
//! background workers feed it through a single `flume` channel:
//!
//! - `prefetch_worker` — one-shot at startup, GETs the eight surfaces
//!   in `Prefetch` so the UI has something to draw before the first
//!   SSE frame arrives.
//! - `sse::spawn_worker` — long-lived, see `crate::sse`.
//! - `telemetry_worker` — periodic poll of `/api/telemetry?since=cursor`
//!   on a 3-second tick. Feeds the Logs route.
//! - `memory_worker` — periodic poll of `/api/memory/recent` on a
//!   10-second tick. Replaces `memory_recent` on each successful
//!   frame so the Knowledge route picks up new drawers without a
//!   manual reload.
//!
//! The gpui side calls [`AppState::pump_events`] inside
//! `cx.spawn(async ...)` to drain the channel and update self via the
//! weak-entity pattern, calling `cx.notify()` after each update so
//! observers redraw.

use std::collections::VecDeque;
use std::time::Duration;

use gpui::{App, Context, Entity};

use crate::api::types::{
    AgentKillsResponse, AgentLaunchRequest, AgentLaunchResponse, AgentModelsResponse,
    AgentProfilesResponse, Bootstrap, DiscoveryReport, GraphSnapshot, MemoryIngestRequest,
    MemoryIngestResponse, MemoryRecentResponse, OllamaKey, OtlpHealth, SseActivityEvent, SseAgent,
    TelemetryEvent, TelemetrySnapshot,
};
use crate::api::Client;
use crate::sse::{self, SseEvent};

/// Cap on the in-memory recent-activity buffer. Tuned for the
/// DashboardHome tile (~5 visible rows + headroom). Large queries
/// that arrive in a single SSE frame still all land here, then age
/// off as new frames arrive.
const ACTIVITY_BUFFER: usize = 64;
/// Cap on the telemetry buffer. The Logs route renders the most-recent
/// 256 events; older ones drop off. A bigger ring would mean the route
/// has to scroll through history, which isn't useful when newer log
/// lines are always more relevant.
const TELEMETRY_BUFFER: usize = 256;
/// Telemetry poll interval. 3s matches the existing web `usePolling.ts`
/// cadence on the React side; tuned to surface logs near-real-time
/// without hammering the server when nothing's happening.
const TELEMETRY_POLL: Duration = Duration::from_secs(3);
/// Memory drawer refresh cadence. Drawers churn slower than telemetry
/// — the typical write rate is "human ingest from CLI", so 10s is fine.
const MEMORY_POLL: Duration = Duration::from_secs(10);

/// Bundle of one-shot prefetch results, all fired on the same OS
/// thread at startup. Each field carries its own `Result` so a dead
/// endpoint doesn't withhold the others — partial state is preferable
/// to a blocked UI.
///
/// Adding a field: extend the struct, the prefetch worker, and the
/// matching `apply` arm; the type-checker keeps the three in sync.
#[derive(Debug)]
pub struct Prefetch {
    pub bootstrap: anyhow::Result<Bootstrap>,
    pub services: anyhow::Result<DiscoveryReport>,
    pub graph: anyhow::Result<GraphSnapshot>,
    pub memory_recent: anyhow::Result<MemoryRecentResponse>,
    pub otlp_health: anyhow::Result<OtlpHealth>,
    pub agent_profiles: anyhow::Result<AgentProfilesResponse>,
    pub agent_kills: anyhow::Result<AgentKillsResponse>,
    pub agent_models: anyhow::Result<AgentModelsResponse>,
    pub ollama_key: anyhow::Result<OllamaKey>,
}

/// One end-to-end app event. The workers ([`spawn_workers`]) multiplex
/// through this single channel so the gpui pump only needs one drain
/// loop.
#[derive(Debug)]
pub enum AppEvent {
    /// One-shot prefetch — see [`Prefetch`]. Boxed because the variant
    /// is ~1.6 KB on stack vs. the next-largest variant's ~64 B; the
    /// indirection keeps the channel cheap (clippy::large_enum_variant).
    Initial(Box<Prefetch>),
    /// Periodic telemetry poll. Carries new events since the last cursor
    /// plus the new cursor; `Err` skips this tick without resetting the
    /// cursor.
    Telemetry(anyhow::Result<TelemetrySnapshot>),
    /// Periodic memory-drawer refresh. Replaces the cached snapshot on
    /// `Ok`; routes `Err` through `last_error` and keeps the previous
    /// snapshot intact.
    MemoryRefresh(anyhow::Result<MemoryRecentResponse>),
    /// Result of a user-initiated `POST /api/memory/ingest` from the
    /// Knowledge-route form. The view stashes a short status line in
    /// `last_ingest`; the worker also fires a follow-up `MemoryRefresh`
    /// on success so the new drawer surfaces immediately.
    MemoryIngestResult(anyhow::Result<MemoryIngestResponse>),
    /// Result of a user-initiated `POST /api/agents/launch` from the
    /// Home-route spawn form. Status flows into `last_launch`; the
    /// agent's output lands in the existing telemetry feed, so no
    /// special streaming needed.
    AgentLaunchResult(anyhow::Result<AgentLaunchResponse>),
    Sse(SseEvent),
}

/// Active dashboard route. Plain enum on `AppState` rather than a hash
/// router (the gpui process is single-window today; deep-linking from
/// the OS would warrant something more elaborate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Route {
    #[default]
    Home,
    Logs,
    System,
    Knowledge,
    Commands,
}

impl Route {
    pub fn label(self) -> &'static str {
        match self {
            Route::Home => "Home",
            Route::Logs => "Logs",
            Route::System => "System",
            Route::Knowledge => "Knowledge",
            Route::Commands => "Commands",
        }
    }

    pub const ALL: [Route; 5] = [
        Route::Home,
        Route::Logs,
        Route::System,
        Route::Knowledge,
        Route::Commands,
    ];
}

#[derive(Default, Debug)]
pub struct AppState {
    pub bootstrap: Option<Bootstrap>,
    pub services: Option<DiscoveryReport>,
    /// Snapshot from `/api/seed-graph`. Fetched once at prefetch time;
    /// the seed graph is static across a serve session, so no SSE
    /// channel for it. Re-fetch on demand once we add a refresh action.
    pub graph: Option<GraphSnapshot>,
    /// Recent memory drawers from `/api/memory/recent`. Fetched once at
    /// prefetch time. Future revs add a periodic refresh / on-demand
    /// re-fetch button.
    pub memory_recent: Option<MemoryRecentResponse>,
    /// One-shot OTLP collector health probe. Surfaces on the System
    /// route as a green/red pill.
    pub otlp_health: Option<OtlpHealth>,
    /// Agent profile registry — directory of declared agent personas.
    pub agent_profiles: Option<AgentProfilesResponse>,
    /// Recent agent kill rows (subprocess SIGKILLs / panics).
    pub agent_kills: Option<AgentKillsResponse>,
    /// Agent model registry — provider × model catalogue.
    pub agent_models: Option<AgentModelsResponse>,
    /// Local Ollama API key state — presence + path metadata, no body.
    pub ollama_key: Option<OllamaKey>,
    pub agents: Vec<SseAgent>,
    pub recent_activity: VecDeque<SseActivityEvent>,
    /// Tail of recent telemetry events (capped at `TELEMETRY_BUFFER`).
    /// Driven by the periodic telemetry poller; not SSE.
    pub telemetry: VecDeque<TelemetryEvent>,
    /// `since=` value to pass on the next telemetry poll. Updated on
    /// every successful frame; left intact on transient failures so we
    /// don't replay events.
    pub telemetry_cursor: u64,
    /// Cumulative number of activity events seen since startup —
    /// not the number of rows in `recent_activity` (that's bounded).
    pub activity_total: u64,
    /// Last unix-seconds timestamp we observed in any incoming event.
    /// Drives the "X seconds ago" hint on the KPI strip.
    pub last_event_ts: Option<i64>,
    /// Most recent error from either worker (string-ified for display).
    pub last_error: Option<String>,
    /// One-line status from the most recent in-window ingest submit.
    /// `Ok` carries a "ingested 1 row · drawer #N" string; `Err`
    /// carries the failure message. Cleared lazily when the user types
    /// in the input again.
    pub last_ingest: Option<Result<String, String>>,
    /// Mirror of `last_ingest` for the Home spawn-agent form.
    pub last_launch: Option<Result<String, String>>,
    /// Currently selected route — driven by the header nav clicks. The
    /// shell view re-renders on `cx.notify` and dispatches body content
    /// based on this value.
    pub route: Route,
    /// Channel back into the worker pump. Set by `state::build`.
    /// Routes that fire one-shot HTTP work (e.g. memory-ingest from
    /// the Knowledge form) `clone()` it and feed an `AppEvent` from a
    /// detached `std::thread`. Optional only because `Default` can't
    /// fabricate a real channel — the unwrap in `submit_ingest` is
    /// guaranteed safe in practice.
    ingest_tx: Option<flume::Sender<AppEvent>>,
    /// Base URL for any `Client` instances spawned from
    /// `submit_ingest`. Mirrors the value passed to `state::build`.
    base_url: Option<String>,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one event. Pure mutation — the caller wraps in
    /// `entity.update(cx, |this, cx| { this.apply(evt); cx.notify(); })`.
    pub fn apply(&mut self, evt: AppEvent) {
        match evt {
            AppEvent::Initial(boxed) => {
                let p = *boxed;
                match p.bootstrap {
                    Ok(b) => self.bootstrap = Some(b),
                    Err(e) => self.last_error = Some(format!("bootstrap: {e}")),
                }
                match p.services {
                    Ok(s) => self.services = Some(s),
                    Err(e) => self.last_error = Some(format!("services: {e}")),
                }
                match p.graph {
                    Ok(g) => self.graph = Some(g),
                    Err(e) => self.last_error = Some(format!("graph: {e}")),
                }
                match p.memory_recent {
                    Ok(m) => self.memory_recent = Some(m),
                    Err(e) => self.last_error = Some(format!("memory_recent: {e}")),
                }
                match p.otlp_health {
                    Ok(o) => self.otlp_health = Some(o),
                    Err(e) => self.last_error = Some(format!("otlp_health: {e}")),
                }
                match p.agent_profiles {
                    Ok(a) => self.agent_profiles = Some(a),
                    Err(e) => self.last_error = Some(format!("agent_profiles: {e}")),
                }
                match p.agent_kills {
                    Ok(a) => self.agent_kills = Some(a),
                    Err(e) => self.last_error = Some(format!("agent_kills: {e}")),
                }
                match p.agent_models {
                    Ok(a) => self.agent_models = Some(a),
                    Err(e) => self.last_error = Some(format!("agent_models: {e}")),
                }
                match p.ollama_key {
                    Ok(k) => self.ollama_key = Some(k),
                    Err(e) => self.last_error = Some(format!("ollama_key: {e}")),
                }
            }
            AppEvent::Sse(SseEvent::Activity(frame)) => {
                self.activity_total = self.activity_total.saturating_add(frame.events.len() as u64);
                if let Some(last) = frame.events.last() {
                    self.last_event_ts = Some(last.ts);
                }
                for evt in frame.events {
                    if self.recent_activity.len() == ACTIVITY_BUFFER {
                        self.recent_activity.pop_front();
                    }
                    self.recent_activity.push_back(evt);
                }
            }
            AppEvent::Sse(SseEvent::Agents(frame)) => {
                self.agents = frame.agents;
            }
            AppEvent::Sse(SseEvent::Unknown { .. }) => {
                // Silent ignore — `crate::sse` already prints to stderr.
            }
            AppEvent::Telemetry(Ok(snapshot)) => {
                self.telemetry_cursor = snapshot.cursor;
                for evt in snapshot.events {
                    if self.telemetry.len() == TELEMETRY_BUFFER {
                        self.telemetry.pop_front();
                    }
                    self.telemetry.push_back(evt);
                }
            }
            AppEvent::Telemetry(Err(e)) => {
                self.last_error = Some(format!("telemetry: {e}"));
            }
            AppEvent::MemoryRefresh(Ok(snapshot)) => {
                self.memory_recent = Some(snapshot);
            }
            AppEvent::MemoryRefresh(Err(e)) => {
                self.last_error = Some(format!("memory_refresh: {e}"));
            }
            AppEvent::MemoryIngestResult(Ok(resp)) => {
                let n = resp.stats.ok;
                let drawer = resp
                    .ingested
                    .first()
                    .map(|e| format!(" · drawer #{}", e.drawer_id))
                    .unwrap_or_default();
                self.last_ingest = Some(Ok(format!("ingested {n} row{}{drawer}", if n == 1 { "" } else { "s" })));
            }
            AppEvent::MemoryIngestResult(Err(e)) => {
                self.last_ingest = Some(Err(format!("ingest failed: {e}")));
            }
            AppEvent::AgentLaunchResult(Ok(resp)) => {
                let pid = resp
                    .pid
                    .map(|p| format!(" pid {p}"))
                    .unwrap_or_default();
                let chars = resp.prompt_chars;
                self.last_launch = Some(Ok(format!(
                    "spawned agent{pid} · {chars} chars · timeout {}s",
                    resp.timeout_secs
                )));
            }
            AppEvent::AgentLaunchResult(Err(e)) => {
                self.last_launch = Some(Err(format!("launch failed: {e}")));
            }
        }
    }

    /// Fire a memory ingest request from a detached thread. Sends an
    /// `AppEvent::MemoryIngestResult` back through the worker channel,
    /// then — on success — fires a follow-up `MemoryRefresh` so the
    /// new drawer appears in the Knowledge list without waiting for
    /// the periodic poller.
    pub fn submit_ingest(&self, req: MemoryIngestRequest) {
        let Some(tx) = self.ingest_tx.clone() else { return };
        let Some(base) = self.base_url.clone() else { return };
        std::thread::Builder::new()
            .name("crabcc-ingest".into())
            .spawn(move || {
                let client = Client::with_base_url(base);
                let result = client.memory_ingest(&req);
                let success = result.is_ok();
                if tx.send(AppEvent::MemoryIngestResult(result)).is_err() {
                    return;
                }
                if success {
                    let refresh = client.memory_recent();
                    let _ = tx.send(AppEvent::MemoryRefresh(refresh));
                }
            })
            .expect("ingest thread spawn");
    }

    /// Fire an agent-launch request from a detached thread. The
    /// agent's stdout is already plumbed into the telemetry feed by
    /// `crabcc serve`, so we don't need a streaming reply — the SSE
    /// pump + telemetry poll surface progress on their own. Result
    /// of the launch (pid + timeout) lands in `last_launch` for the
    /// Home-route status line.
    pub fn submit_launch(&self, req: AgentLaunchRequest) {
        let Some(tx) = self.ingest_tx.clone() else { return };
        let Some(base) = self.base_url.clone() else { return };
        std::thread::Builder::new()
            .name("crabcc-launch".into())
            .spawn(move || {
                let client = Client::with_base_url(base);
                let _ = tx.send(AppEvent::AgentLaunchResult(client.agent_launch(&req)));
            })
            .expect("launch thread spawn");
    }

    pub fn set_route(&mut self, route: Route) {
        self.route = route;
    }

    pub fn agents_running(&self) -> u32 {
        use crate::api::types::AgentStatus;
        self.agents
            .iter()
            .filter(|a| matches!(a.status, AgentStatus::Running))
            .count() as u32
    }

    pub fn services_reachable(&self) -> Option<(u32, u32)> {
        let report = self.services.as_ref()?;
        let total = report.services.len() as u32;
        let up = report.services.iter().filter(|s| s.reachable).count() as u32;
        Some((up, total))
    }

    pub fn pump_events(handle: &Entity<Self>, rx: flume::Receiver<AppEvent>, cx: &mut Context<Self>) {
        let weak = handle.downgrade();
        cx.spawn(async move |_, cx| {
            while let Ok(evt) = rx.recv_async().await {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                this.update(cx, |this, cx| {
                    this.apply(evt);
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

/// Spawn the four background workers and return both the receiver
/// (drained by the gpui pump via [`AppState::pump_events`]) and a
/// cloned sender — the latter is stashed on `AppState::ingest_tx` so
/// one-shot UI-driven work (memory ingest, future agent spawn) can
/// post events back into the same channel from a detached thread.
///
/// Workers run on their own OS threads — no async runtime needed, and
/// [`flume::Receiver::recv_async`] works inside gpui's smol-flavored
/// `cx.spawn`.
pub fn spawn_workers(base_url: &str) -> (flume::Sender<AppEvent>, flume::Receiver<AppEvent>) {
    let (tx, rx) = flume::unbounded::<AppEvent>();

    // One-shot prefetch — bootstrap + services + seed-graph all on the
    // same thread. The seed-graph response is ~20 KB / 96 nodes today
    // so an extra HTTP round-trip at startup is fine; promote to a
    // background-on-demand fetch if the graph grows large.
    {
        let tx = tx.clone();
        let base = base_url.to_string();
        std::thread::Builder::new()
            .name("crabcc-prefetch".into())
            .spawn(move || {
                let client = Client::with_base_url(base);
                let prefetch = Prefetch {
                    bootstrap: client.bootstrap(),
                    services: client.services(),
                    graph: client.seed_graph(),
                    memory_recent: client.memory_recent(),
                    otlp_health: client.otlp_health(),
                    agent_profiles: client.agent_profiles(),
                    agent_kills: client.agent_kills(),
                    agent_models: client.agent_models(),
                    ollama_key: client.ollama_key(),
                };
                // Receiver disconnect is fine — app shutdown raced us.
                let _ = tx.send(AppEvent::Initial(Box::new(prefetch)));
            })
            .expect("prefetch thread spawn");
    }

    // Long-lived SSE pump. Wrap each `SseEvent` in `AppEvent::Sse` on
    // its way out so `AppState::apply` only has one match arm shape.
    let sse_rx = sse::spawn_worker(base_url);
    {
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("crabcc-sse-bridge".into())
            .spawn(move || {
                while let Ok(evt) = sse_rx.recv() {
                    if tx.send(AppEvent::Sse(evt)).is_err() {
                        return;
                    }
                }
            })
            .expect("sse bridge thread spawn");
    }

    // Long-lived telemetry poller. Synchronous loop on its own thread,
    // sleeps `TELEMETRY_POLL` between attempts. We don't track the
    // cursor inside the worker — the gpui-side `AppState::apply` owns
    // it, but the worker passes the latest known cursor back through
    // its own captured copy so we don't reset on transient failures.
    {
        let tx = tx.clone();
        let base = base_url.to_string();
        std::thread::Builder::new()
            .name("crabcc-telemetry".into())
            .spawn(move || {
                let client = Client::with_base_url(base);
                let mut cursor: i64 = 0;
                loop {
                    if tx.is_disconnected() {
                        return;
                    }
                    let result = client.telemetry(Some(cursor), 100);
                    if let Ok(snapshot) = &result {
                        cursor = snapshot.cursor as i64;
                    }
                    if tx.send(AppEvent::Telemetry(result)).is_err() {
                        return;
                    }
                    std::thread::sleep(TELEMETRY_POLL);
                }
            })
            .expect("telemetry thread spawn");
    }

    // Long-lived memory-drawer poller. Slower cadence than telemetry —
    // drawer creation is a human-driven event from `crabcc memory
    // ingest`, so 10s is plenty. Mirrors the telemetry pattern: GET,
    // send, sleep, repeat.
    {
        let tx = tx.clone();
        let base = base_url.to_string();
        std::thread::Builder::new()
            .name("crabcc-memory-poll".into())
            .spawn(move || {
                let client = Client::with_base_url(base);
                loop {
                    if tx.is_disconnected() {
                        return;
                    }
                    // First tick fires immediately after `MEMORY_POLL`
                    // — the prefetch worker already covered the cold
                    // path, so we can sleep before the first GET to
                    // skip a redundant fetch at startup.
                    std::thread::sleep(MEMORY_POLL);
                    if tx.send(AppEvent::MemoryRefresh(client.memory_recent())).is_err() {
                        return;
                    }
                }
            })
            .expect("memory-poll thread spawn");
    }

    (tx, rx)
}

/// Returns the AppState entity wired up with workers. Call from inside
/// a gpui context (e.g. `cx.new(|cx| build(cx, base))`).
pub fn build(cx: &mut Context<AppState>, base_url: &str) -> AppState {
    let (tx, rx) = spawn_workers(base_url);
    let entity = cx.entity();
    AppState::pump_events(&entity, rx, cx);
    AppState {
        ingest_tx: Some(tx),
        base_url: Some(base_url.to_string()),
        ..AppState::new()
    }
}

// Suppress dead-code lint for the unused `_app` arg until A.5 needs it.
#[allow(dead_code)]
fn _app_marker(_app: &App) {}
