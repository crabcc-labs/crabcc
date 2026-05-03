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

use gpui::{App, Context, Entity, SharedString};
use tracing::{debug, info};

use crate::api::types::{
    AgentKillResponse, AgentKillsResponse, AgentLaunchRequest, AgentLaunchResponse, AgentLog,
    AgentModelsResponse, AgentProfilesResponse, Bootstrap, DiscoveryReport, GraphSnapshot,
    MemoryIngestRequest, MemoryIngestResponse, MemoryRecentResponse, OllamaKey, OtlpHealth,
    SseActivityEvent, SseAgent, TelemetryEvent, TelemetrySnapshot,
};
use crate::api::Client;
use crate::sse::{self, SseEvent};
use crate::toasts::{Toast, ToastLevel, MAX_VISIBLE_TOASTS};

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
    /// Result of a user-initiated `POST /api/agents/{id}/kill`. The
    /// agents-list refresh comes from the existing SSE pump (the
    /// server emits an `agents` frame on each kill), so we don't fire
    /// a follow-up GET here.
    AgentKillResult(anyhow::Result<AgentKillResponse>),
    /// Result of a one-shot `GET /api/agents/{id}/log?since=0`, dispatched
    /// from the Agents route when the user clicks an agent card. The
    /// id carries through so the route can ignore late results for an
    /// agent the user already deselected (cheap stale-check).
    AgentLogResult {
        id: SharedString,
        result: anyhow::Result<AgentLog>,
    },
    Sse(SseEvent),
}

/// One-slot result of the most recent `agent_log` fetch. Carries the
/// agent id alongside the body so a stale fetch (user already
/// deselected) can be filtered out at render time without racing the
/// dispatch path.
#[derive(Debug)]
pub struct AgentLogState {
    pub id: SharedString,
    pub result: anyhow::Result<AgentLog>,
}

/// Active dashboard route. Plain enum on `AppState` rather than a hash
/// router (the gpui process is single-window today; deep-linking from
/// the OS would warrant something more elaborate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Route {
    #[default]
    Home,
    Agents,
    Logs,
    System,
    Knowledge,
    Commands,
}

impl Route {
    pub fn label(self) -> &'static str {
        match self {
            Route::Home => "Home",
            Route::Agents => "Agents",
            Route::Logs => "Logs",
            Route::System => "System",
            Route::Knowledge => "Knowledge",
            Route::Commands => "Commands",
        }
    }

    pub const ALL: [Route; 6] = [
        Route::Home,
        Route::Agents,
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
    /// Mirror of `last_ingest` for the per-agent kill button.
    pub last_kill: Option<Result<String, String>>,
    /// Most recent per-agent log fetch (Agents route). Holds both the
    /// selected agent id and the result body so the view can render a
    /// "stale" warning if the id no longer matches the active selection.
    /// Cleared explicitly when the user deselects.
    pub agent_log: Option<AgentLogState>,
    /// Currently selected route — driven by the header nav clicks. The
    /// shell view re-renders on `cx.notify` and dispatches body content
    /// based on this value.
    pub route: Route,
    /// Worker plumbing for one-shot HTTP submits — see [`WorkerHandles`].
    /// `Option` only because `Default` can't fabricate a real flume
    /// channel; `state::build` populates it before the view ever
    /// reads. The two fields used to live here directly as paired
    /// `Option<_>`s; the review in #236 flagged that as a smell — they
    /// always come from `state::build` together, so they should
    /// always *be* together.
    workers: Option<WorkerHandles>,
    /// Active in-window toasts (track C.0). Newest-first, capped
    /// at [`MAX_VISIBLE_TOASTS`] — over-cap pushes evict the oldest
    /// (back of the deque). The render component
    /// (`crate::toasts::ToastStrip`) reads this directly and renders
    /// up to the cap.
    pub toasts: VecDeque<Toast>,
    /// Monotonic counter for toast ids. Persists across pushes /
    /// evictions so dismiss-by-id never races a reused id.
    next_toast_id: u64,
    /// Edge-trigger sentinel: when the telemetry poll is failing,
    /// holds the id of the persistent Warning toast so we don't
    /// spam a fresh one on every poll cycle. Cleared (and the
    /// toast dismissed) when a poll succeeds.
    telemetry_warning_id: Option<u64>,
    /// Same edge-trigger sentinel for the memory-recent poll.
    memory_warning_id: Option<u64>,
    /// User-toggled mute. When `true`, [`AppState::push_toast`]
    /// returns the next id but skips the actual push, so emit-paths
    /// keep working (sentinels still get a non-zero id, dismiss
    /// stays idempotent) but the strip shows nothing. Toggled by
    /// the header bell — see `Shell::render`.
    pub toasts_muted: bool,
}

/// Plumbing the route entities use to fire detached HTTP requests
/// back through the worker channel. Both fields are populated as a
/// unit by `state::build` and never re-set after — the bundle exists
/// so each `submit_*` method needs only one `let Some(handles) = …`
/// guard instead of two paired-Optional unwraps.
#[derive(Debug, Clone)]
pub struct WorkerHandles {
    /// Channel back into the worker pump. `clone()`-cheap (flume's
    /// senders are MPSC handles wrapping an `Arc`).
    pub tx: flume::Sender<AppEvent>,
    /// Base URL for `Client` instances spawned from `submit_*`.
    /// Mirrors the value `state::build` was called with.
    pub base_url: String,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a new toast into the strip. Newest goes to the front;
    /// when the buffer hits [`MAX_VISIBLE_TOASTS`] the oldest (back)
    /// is evicted so the strip never grows past the cap. Returns the
    /// assigned toast id so the caller can target a future
    /// [`dismiss_toast`] without scanning.
    ///
    /// Also opportunistically GCs expired toasts before push so the
    /// deque doesn't carry stale rows past their auto-dismiss
    /// interval. Combined with [`ToastStrip`]'s render-time skip,
    /// expired toasts disappear visually within the next render
    /// trigger after their interval lapses.
    ///
    /// When [`AppState::toasts_muted`] is `true`, returns a fresh
    /// id but does NOT enqueue the toast — emit paths can keep
    /// using the returned id as a sentinel (e.g.
    /// `telemetry_warning_id`) and `dismiss_toast` stays idempotent
    /// against an absent toast.
    pub fn push_toast(&mut self, level: ToastLevel, message: impl Into<SharedString>) -> u64 {
        let id = self.next_toast_id;
        self.next_toast_id = self.next_toast_id.wrapping_add(1);
        if self.toasts_muted {
            // Sentinel-only: edge-trigger code (slice 3) needs a
            // unique id even when muted, so dismiss-on-recover
            // doesn't accidentally target somebody else's toast.
            return id;
        }
        self.gc_expired_toasts();
        // Wall-clock proxy: prefer the latest observed event ts so
        // the timestamp matches other UI surfaces (KPI strip "X
        // seconds ago"). Falls back to 0 before the first event.
        let created_at = self.last_event_ts.unwrap_or(0);
        if self.toasts.len() >= MAX_VISIBLE_TOASTS {
            self.toasts.pop_back();
        }
        self.toasts.push_front(Toast {
            id,
            level,
            message: message.into(),
            created_at,
        });
        id
    }

    /// Flip the mute state. When transitioning false→true, also
    /// clears any currently-visible toasts — the user's intent is
    /// "stop showing me things", so leaving stale rows on screen
    /// would be a surprise.
    pub fn toggle_toast_mute(&mut self) {
        self.toasts_muted = !self.toasts_muted;
        if self.toasts_muted {
            self.toasts.clear();
        }
    }

    /// Dismiss the toast with the given id. No-op if it was already
    /// evicted by an over-cap push or a prior dismiss.
    pub fn dismiss_toast(&mut self, id: u64) {
        self.toasts.retain(|t| t.id != id);
    }

    /// Drop toasts whose [`ToastLevel::dismiss_after_secs`] interval
    /// has elapsed since their `created_at`. Persistent levels
    /// (warning / danger return `None`) stay until manually
    /// dismissed. Cheap retain — runs on every `push_toast`.
    pub fn gc_expired_toasts(&mut self) {
        let Some(now) = self.last_event_ts else {
            return;
        };
        self.toasts.retain(|t| t.is_active(now));
    }

    /// Apply one event. Pure mutation — the caller wraps in
    /// `entity.update(cx, |this, cx| { this.apply(evt); cx.notify(); })`.
    pub fn apply(&mut self, evt: AppEvent) {
        match evt {
            AppEvent::Initial(boxed) => {
                let p = *boxed;
                // Collect prefetch error sources so we can surface
                // one summary toast at the end of the arm rather
                // than spamming nine separate toasts (cap=5 would
                // evict half of them anyway, and the user only
                // needs one signal about prefetch health).
                let mut prefetch_errs: Vec<&'static str> = Vec::new();
                match p.bootstrap {
                    Ok(b) => self.bootstrap = Some(b),
                    Err(e) => {
                        self.last_error = Some(format!("bootstrap: {e}"));
                        prefetch_errs.push("bootstrap");
                    }
                }
                match p.services {
                    Ok(s) => self.services = Some(s),
                    Err(e) => {
                        self.last_error = Some(format!("services: {e}"));
                        prefetch_errs.push("services");
                    }
                }
                match p.graph {
                    Ok(g) => self.graph = Some(g),
                    Err(e) => {
                        self.last_error = Some(format!("graph: {e}"));
                        prefetch_errs.push("graph");
                    }
                }
                match p.memory_recent {
                    Ok(m) => self.memory_recent = Some(m),
                    Err(e) => {
                        self.last_error = Some(format!("memory_recent: {e}"));
                        prefetch_errs.push("memory_recent");
                    }
                }
                match p.otlp_health {
                    Ok(o) => self.otlp_health = Some(o),
                    Err(e) => {
                        self.last_error = Some(format!("otlp_health: {e}"));
                        prefetch_errs.push("otlp_health");
                    }
                }
                match p.agent_profiles {
                    Ok(a) => self.agent_profiles = Some(a),
                    Err(e) => {
                        self.last_error = Some(format!("agent_profiles: {e}"));
                        prefetch_errs.push("agent_profiles");
                    }
                }
                match p.agent_kills {
                    Ok(a) => self.agent_kills = Some(a),
                    Err(e) => {
                        self.last_error = Some(format!("agent_kills: {e}"));
                        prefetch_errs.push("agent_kills");
                    }
                }
                match p.agent_models {
                    Ok(a) => self.agent_models = Some(a),
                    Err(e) => {
                        self.last_error = Some(format!("agent_models: {e}"));
                        prefetch_errs.push("agent_models");
                    }
                }
                match p.ollama_key {
                    Ok(k) => self.ollama_key = Some(k),
                    Err(e) => {
                        self.last_error = Some(format!("ollama_key: {e}"));
                        prefetch_errs.push("ollama_key");
                    }
                }
                if !prefetch_errs.is_empty() {
                    let msg = if prefetch_errs.len() == 1 {
                        format!("prefetch failed: {}", prefetch_errs[0])
                    } else {
                        format!(
                            "prefetch: {} sources failed ({})",
                            prefetch_errs.len(),
                            prefetch_errs.join(", ")
                        )
                    };
                    self.push_toast(ToastLevel::Danger, msg);
                }
            }
            AppEvent::Sse(SseEvent::Activity(frame)) => {
                self.activity_total = self
                    .activity_total
                    .saturating_add(frame.events.len() as u64);
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
            AppEvent::Sse(SseEvent::Agents(mut frame)) => {
                // Pre-compute the per-agent click-target gpui element
                // ids once here, instead of per agent per render. See
                // `AgentDerived` — the `format!()` cost is paid once
                // per SSE update.
                for agent in &mut frame.agents {
                    agent.derived = crate::api::types::AgentDerived::from_id(&agent.id);
                }
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
                // Edge-trigger recovery: if the prior poll(s) had
                // raised a Warning, dismiss it and pop a Success
                // "recovered" toast so the user knows the channel
                // is back without having to compare two negatives.
                if let Some(id) = self.telemetry_warning_id.take() {
                    self.dismiss_toast(id);
                    self.push_toast(ToastLevel::Success, "telemetry recovered");
                }
            }
            AppEvent::Telemetry(Err(e)) => {
                self.last_error = Some(format!("telemetry: {e}"));
                // Edge-trigger fail: emit one persistent Warning
                // toast on the first failure and remember its id.
                // Subsequent failures don't spam — they wait for
                // either a recovery or a manual dismiss.
                if self.telemetry_warning_id.is_none() {
                    let id = self.push_toast(ToastLevel::Warning, format!("telemetry: {e}"));
                    self.telemetry_warning_id = Some(id);
                }
            }
            AppEvent::MemoryRefresh(Ok(snapshot)) => {
                self.memory_recent = Some(snapshot);
                // Same edge-trigger recovery as Telemetry — see
                // there for the rationale.
                if let Some(id) = self.memory_warning_id.take() {
                    self.dismiss_toast(id);
                    self.push_toast(ToastLevel::Success, "memory poll recovered");
                }
            }
            AppEvent::MemoryRefresh(Err(e)) => {
                self.last_error = Some(format!("memory_refresh: {e}"));
                if self.memory_warning_id.is_none() {
                    let id = self.push_toast(ToastLevel::Warning, format!("memory_refresh: {e}"));
                    self.memory_warning_id = Some(id);
                }
            }
            AppEvent::MemoryIngestResult(Ok(resp)) => {
                let n = resp.stats.ok;
                let drawer = resp
                    .ingested
                    .first()
                    .map(|e| format!(" · drawer #{}", e.drawer_id))
                    .unwrap_or_default();
                let msg = format!("ingested {n} row{}{drawer}", if n == 1 { "" } else { "s" });
                self.push_toast(ToastLevel::Success, msg.clone());
                self.last_ingest = Some(Ok(msg));
            }
            AppEvent::MemoryIngestResult(Err(e)) => {
                let msg = format!("ingest failed: {e}");
                self.push_toast(ToastLevel::Danger, msg.clone());
                self.last_ingest = Some(Err(msg));
            }
            AppEvent::AgentLaunchResult(Ok(resp)) => {
                let pid = resp.pid.map(|p| format!(" pid {p}")).unwrap_or_default();
                let chars = resp.prompt_chars;
                let msg = format!(
                    "spawned agent{pid} · {chars} chars · timeout {}s",
                    resp.timeout_secs
                );
                // Primary level (8s) — agent spawns are the
                // operator's most-watched event, deserve the longer
                // visible window.
                self.push_toast(ToastLevel::Primary, msg.clone());
                self.last_launch = Some(Ok(msg));
            }
            AppEvent::AgentLaunchResult(Err(e)) => {
                let msg = format!("launch failed: {e}");
                self.push_toast(ToastLevel::Danger, msg.clone());
                self.last_launch = Some(Err(msg));
            }
            AppEvent::AgentKillResult(Ok(resp)) => {
                let pid = resp.pid.map(|p| format!(" pid {p}")).unwrap_or_default();
                let note = resp
                    .note
                    .as_ref()
                    .map(|n| format!(" · {n}"))
                    .unwrap_or_default();
                let signal = if resp.signaled {
                    "SIGKILL sent"
                } else {
                    "no signal"
                };
                let msg = format!("kill {} →{pid} · {signal}{note}", resp.id);
                // Info level (3s) — kills are routine; the green
                // tick on the agent card already conveys completion.
                self.push_toast(ToastLevel::Info, msg.clone());
                self.last_kill = Some(Ok(msg));
            }
            AppEvent::AgentKillResult(Err(e)) => {
                let msg = format!("kill failed: {e}");
                self.push_toast(ToastLevel::Danger, msg.clone());
                self.last_kill = Some(Err(msg));
            }
            AppEvent::AgentLogResult { id, result } => {
                self.agent_log = Some(AgentLogState { id, result });
            }
        }
    }

    /// Fire a memory ingest request from a detached thread. Sends an
    /// `AppEvent::MemoryIngestResult` back through the worker channel,
    /// then — on success — fires a follow-up `MemoryRefresh` so the
    /// new drawer appears in the Knowledge list without waiting for
    /// the periodic poller.
    pub fn submit_ingest(&self, req: MemoryIngestRequest) {
        let Some(handles) = self.workers.clone() else {
            return;
        };
        let WorkerHandles { tx, base_url } = handles;
        std::thread::Builder::new()
            .name("crabcc-ingest".into())
            .spawn(move || {
                debug!(target: "crabcc::state", thread = "ingest", "starting");
                let client = Client::with_base_url(base_url);
                let result = client.memory_ingest(&req);
                let success = result.is_ok();
                if try_send_app_event(&tx, AppEvent::MemoryIngestResult(result)).is_err() {
                    debug!(target: "crabcc::state", thread = "ingest", "exiting (rx dropped)");
                    return;
                }
                if success {
                    let refresh = client.memory_recent();
                    let _ = try_send_app_event(&tx, AppEvent::MemoryRefresh(refresh));
                }
                debug!(target: "crabcc::state", thread = "ingest", "exiting (done)");
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
        let Some(handles) = self.workers.clone() else {
            return;
        };
        let WorkerHandles { tx, base_url } = handles;
        std::thread::Builder::new()
            .name("crabcc-launch".into())
            .spawn(move || {
                debug!(target: "crabcc::state", thread = "launch", "starting");
                let client = Client::with_base_url(base_url);
                let _ =
                    try_send_app_event(&tx, AppEvent::AgentLaunchResult(client.agent_launch(&req)));
                debug!(target: "crabcc::state", thread = "launch", "exiting");
            })
            .expect("launch thread spawn");
    }

    /// SIGKILL a running agent by id. The server emits an `agents`
    /// SSE frame on each kill, so the running-agents list refreshes
    /// itself — no follow-up fetch needed here.
    pub fn submit_kill(&self, id: SharedString) {
        let Some(handles) = self.workers.clone() else {
            return;
        };
        let WorkerHandles { tx, base_url } = handles;
        std::thread::Builder::new()
            .name("crabcc-kill".into())
            .spawn(move || {
                debug!(target: "crabcc::state", thread = "kill", agent_id = %id, "starting");
                let client = Client::with_base_url(base_url);
                let _ = try_send_app_event(&tx, AppEvent::AgentKillResult(client.agent_kill(&id)));
                debug!(target: "crabcc::state", thread = "kill", agent_id = %id, "exiting");
            })
            .expect("kill thread spawn");
    }

    /// One-shot fetch of an agent's stdout tail. The server's `since`
    /// param is byte-offset; passing 0 returns the whole buffer (capped
    /// at the server's window — see crabcc-viz `Client.agentLog`).
    /// Result lands in `agent_log` keyed by id; the Agents route reads
    /// it on next render.
    pub fn submit_agent_log(&self, id: SharedString, since: u64) {
        let Some(handles) = self.workers.clone() else {
            return;
        };
        let WorkerHandles { tx, base_url } = handles;
        std::thread::Builder::new()
            .name("crabcc-agent-log".into())
            .spawn(move || {
                debug!(target: "crabcc::state", thread = "agent-log", agent_id = %id, since, "starting");
                let client = Client::with_base_url(base_url);
                let result = client.agent_log(&id, since);
                let _ = try_send_app_event(&tx, AppEvent::AgentLogResult { id, result });
            })
            .expect("agent-log thread spawn");
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

    pub fn pump_events(
        handle: &Entity<Self>,
        rx: flume::Receiver<AppEvent>,
        cx: &mut Context<Self>,
    ) {
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
/// Bounded buffer for the multiplexed `AppEvent` channel. Four
/// background workers (prefetch + SSE bridge + telemetry poll +
/// memory poll) plus the four UI submit paths funnel through this
/// channel; the gpui pump drains it on the main thread. Cap of 512
/// is ~3 minutes of runway at the union of typical worker rates;
/// overflow (a stuck pump) logs a warn-level line and drops the
/// individual event rather than block any worker. See the
/// `try_send_app_event` helper for the policy.
const APP_CHANNEL_CAP: usize = 512;

/// Best-effort `AppEvent` send. Drops the event (with a warn log)
/// if the channel is full — preferable to blocking a worker thread
/// on a stuck pump. Returns `Ok(())` on successful send, `Err(())`
/// when the receiver has been dropped (caller should treat as
/// shutdown signal).
fn try_send_app_event(tx: &flume::Sender<AppEvent>, evt: AppEvent) -> Result<(), ()> {
    match tx.try_send(evt) {
        Ok(()) => Ok(()),
        Err(flume::TrySendError::Disconnected(_)) => Err(()),
        Err(flume::TrySendError::Full(_)) => {
            tracing::warn!(
                target: "crabcc::state",
                cap = APP_CHANNEL_CAP,
                "app-event channel full, dropping event"
            );
            // We deliberately don't propagate as Err here — the
            // channel is still alive, just saturated. Caller treats
            // the same as a successful send for control-flow purposes.
            Ok(())
        }
    }
}

pub fn spawn_workers(base_url: &str) -> (flume::Sender<AppEvent>, flume::Receiver<AppEvent>) {
    let (tx, rx) = flume::bounded::<AppEvent>(APP_CHANNEL_CAP);

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
                debug!(target: "crabcc::state", thread = "prefetch", "starting");
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
                let _ = try_send_app_event(&tx, AppEvent::Initial(Box::new(prefetch)));
                debug!(target: "crabcc::state", thread = "prefetch", "exiting");
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
                info!(target: "crabcc::state", thread = "sse-bridge", "starting");
                while let Ok(evt) = sse_rx.recv() {
                    if try_send_app_event(&tx, AppEvent::Sse(evt)).is_err() {
                        info!(target: "crabcc::state", thread = "sse-bridge", "exiting (rx dropped)");
                        return;
                    }
                }
                info!(target: "crabcc::state", thread = "sse-bridge", "exiting (sse channel closed)");
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
                info!(target: "crabcc::state", thread = "telemetry", "starting");
                let client = Client::with_base_url(base);
                let mut cursor: i64 = 0;
                loop {
                    if tx.is_disconnected() {
                        info!(target: "crabcc::state", thread = "telemetry", "exiting (rx dropped)");
                        return;
                    }
                    let result = client.telemetry(Some(cursor), 100);
                    if let Ok(snapshot) = &result {
                        cursor = snapshot.cursor as i64;
                    }
                    if try_send_app_event(&tx, AppEvent::Telemetry(result)).is_err() {
                        info!(target: "crabcc::state", thread = "telemetry", "exiting (send fail)");
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
                info!(target: "crabcc::state", thread = "memory-poll", "starting");
                let client = Client::with_base_url(base);
                loop {
                    if tx.is_disconnected() {
                        info!(target: "crabcc::state", thread = "memory-poll", "exiting (rx dropped)");
                        return;
                    }
                    // First tick fires immediately after `MEMORY_POLL`
                    // — the prefetch worker already covered the cold
                    // path, so we can sleep before the first GET to
                    // skip a redundant fetch at startup.
                    std::thread::sleep(MEMORY_POLL);
                    if tx
                        .send(AppEvent::MemoryRefresh(client.memory_recent()))
                        .is_err()
                    {
                        info!(target: "crabcc::state", thread = "memory-poll", "exiting (send fail)");
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
        workers: Some(WorkerHandles {
            tx,
            base_url: base_url.to_string(),
        }),
        ..AppState::new()
    }
}

// Suppress dead-code lint for the unused `_app` arg until A.5 needs it.
#[allow(dead_code)]
fn _app_marker(_app: &App) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_toast_caps_at_max_and_evicts_oldest() {
        // Six pushes against a cap of 5 — newest five must be
        // visible newest-first; the very first push must be gone.
        let mut s = AppState::new();
        let first = s.push_toast(ToastLevel::Info, "first");
        for n in 1..=5 {
            s.push_toast(ToastLevel::Info, format!("n={n}"));
        }
        assert_eq!(s.toasts.len(), MAX_VISIBLE_TOASTS);
        assert_eq!(s.toasts.front().unwrap().message, "n=5");
        assert_eq!(s.toasts.back().unwrap().message, "n=1");
        // Dismiss-by-id of the evicted toast is a no-op (must not
        // panic).
        s.dismiss_toast(first);
        assert_eq!(s.toasts.len(), MAX_VISIBLE_TOASTS);
    }

    #[test]
    fn dismiss_removes_only_the_targeted_toast() {
        let mut s = AppState::new();
        let a = s.push_toast(ToastLevel::Info, "a");
        let b = s.push_toast(ToastLevel::Warning, "b");
        let c = s.push_toast(ToastLevel::Danger, "c");
        s.dismiss_toast(b);
        assert_eq!(s.toasts.len(), 2);
        assert!(s.toasts.iter().any(|t| t.id == a));
        assert!(s.toasts.iter().any(|t| t.id == c));
        assert!(!s.toasts.iter().any(|t| t.id == b));
    }

    #[test]
    fn gc_drops_expired_keeps_persistent() {
        // ts t=0 → push two toasts; advance to t=10 → only the
        // persistent (Warning, no dismiss interval) survives, the
        // Info toast (3s window) is GC'd.
        let mut s = AppState::new();
        s.last_event_ts = Some(0);
        s.push_toast(ToastLevel::Info, "ephemeral");
        s.push_toast(ToastLevel::Warning, "persistent");
        assert_eq!(s.toasts.len(), 2);
        s.last_event_ts = Some(10);
        s.gc_expired_toasts();
        assert_eq!(s.toasts.len(), 1);
        assert_eq!(s.toasts.front().unwrap().message, "persistent");
    }

    #[test]
    fn gc_is_noop_before_first_event() {
        // Before any event has been observed, last_event_ts is None
        // — GC must not panic and must not drop anything (we have
        // no clock to compare against).
        let mut s = AppState::new();
        s.push_toast(ToastLevel::Info, "x");
        s.push_toast(ToastLevel::Success, "y");
        s.gc_expired_toasts();
        assert_eq!(s.toasts.len(), 2);
    }

    #[test]
    fn push_toast_gcs_before_appending() {
        // After the auto-dismiss interval lapses, a new push should
        // GC the expired entries first so the cap-eviction behaviour
        // kicks in only on truly active toasts.
        let mut s = AppState::new();
        s.last_event_ts = Some(0);
        for n in 0..5 {
            s.push_toast(ToastLevel::Info, format!("n={n}"));
        }
        assert_eq!(s.toasts.len(), 5);
        // Advance past the 3s Info window — every prior toast is
        // expired. New push GCs them, then appends just the new one.
        s.last_event_ts = Some(10);
        s.push_toast(ToastLevel::Success, "fresh");
        assert_eq!(s.toasts.len(), 1);
        assert_eq!(s.toasts.front().unwrap().message, "fresh");
    }

    #[test]
    fn telemetry_failure_emits_one_warning_then_dedupes() {
        // First failure pops a Warning toast and stores the id.
        // Subsequent failures must NOT spam — the deque stays at
        // length 1 even after multiple Err arms apply.
        let mut s = AppState::new();
        s.last_event_ts = Some(0);
        s.apply(AppEvent::Telemetry(Err(anyhow::anyhow!("boom"))));
        assert_eq!(s.toasts.len(), 1);
        assert!(s.telemetry_warning_id.is_some());
        let original_id = s.telemetry_warning_id.unwrap();
        // Second failure — id must NOT change, deque must stay at 1.
        s.apply(AppEvent::Telemetry(Err(anyhow::anyhow!("still boom"))));
        assert_eq!(s.toasts.len(), 1);
        assert_eq!(s.telemetry_warning_id, Some(original_id));
    }

    #[test]
    fn mute_blocks_push_but_returns_unique_id() {
        // Muted state: push_toast still hands out unique ids (so
        // edge-trigger sentinels keep working), but no toast lands
        // in the deque.
        let mut s = AppState::new();
        s.toasts_muted = true;
        let a = s.push_toast(ToastLevel::Info, "hidden a");
        let b = s.push_toast(ToastLevel::Warning, "hidden b");
        assert_ne!(a, b, "ids must stay unique even when muted");
        assert!(s.toasts.is_empty(), "muted pushes must not enqueue");
        // Dismiss-by-id of the never-pushed toast is a no-op (must
        // not panic). The contract: emit-paths pretend the push
        // succeeded; dismiss tolerates the absence.
        s.dismiss_toast(a);
        s.dismiss_toast(b);
        assert!(s.toasts.is_empty());
    }

    #[test]
    fn toggle_mute_clears_visible_toasts_on_entry() {
        // User intent of "mute" = "stop showing me things now". A
        // half-finished pile of stale rows would surprise the user.
        let mut s = AppState::new();
        s.last_event_ts = Some(0);
        s.push_toast(ToastLevel::Info, "x");
        s.push_toast(ToastLevel::Warning, "y");
        assert_eq!(s.toasts.len(), 2);
        s.toggle_toast_mute();
        assert!(s.toasts_muted);
        assert!(s.toasts.is_empty());
        // Unmuting alone doesn't re-emit anything — toasts only
        // come from new push_toast calls.
        s.toggle_toast_mute();
        assert!(!s.toasts_muted);
        assert!(s.toasts.is_empty());
    }

    #[test]
    fn telemetry_recovery_dismisses_warning_and_pops_success() {
        // After a fail then a recovery, the Warning is gone and a
        // Success toast appears in its place.
        let mut s = AppState::new();
        s.last_event_ts = Some(0);
        s.apply(AppEvent::Telemetry(Err(anyhow::anyhow!("boom"))));
        assert_eq!(s.toasts.len(), 1);
        assert!(matches!(
            s.toasts.front().unwrap().level,
            ToastLevel::Warning
        ));
        // Recover.
        s.apply(AppEvent::Telemetry(Ok(
            crate::api::types::TelemetrySnapshot {
                cursor: 1,
                events: vec![],
                source: crate::api::types::TelemetrySource {
                    path: String::new(),
                    lines_read: 0,
                    bytes: 0,
                    exists: true,
                },
            },
        )));
        assert!(s.telemetry_warning_id.is_none());
        // Newest is the Success "telemetry recovered" toast.
        assert_eq!(s.toasts.len(), 1);
        let front = s.toasts.front().unwrap();
        assert!(matches!(front.level, ToastLevel::Success));
        assert_eq!(front.message, "telemetry recovered");
    }
}
