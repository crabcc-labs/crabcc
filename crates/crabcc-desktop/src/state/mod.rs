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
use std::sync::{Arc, RwLock};

use gpui::{Context, Entity, SharedString};
use tracing::debug;

use crate::api::types::{
    AgentKillResponse, AgentKillsResponse, AgentLaunchRequest, AgentLaunchResponse, AgentLog,
    AgentModelsResponse, AgentProfilesResponse, AgentStatus, Bootstrap, DiscoveryReport,
    GraphSnapshot, MemoryGraphResponse, MemoryIngestRequest, MemoryIngestResponse,
    MemoryRecentResponse, OllamaKey, OtlpHealth, SseActivityEvent, SseAgent, TelemetryEvent,
    TelemetrySnapshot,
};
use crate::api::Client;
use crate::inspector::{CallEvent, INSPECTOR_RING_CAP};
use crate::sse::SseEvent;
use crate::toasts::{Toast, ToastLevel, MAX_VISIBLE_TOASTS};

use self::workers::{run_command, try_send_app_event};

pub(crate) mod build;
pub(crate) mod workers;
pub use build::build;

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
    /// One-shot result of [`AppState::submit_memory_graph`]. Replaces
    /// `memory_graph` on success; `Err` lands in `memory_graph_error`
    /// for inline display without disturbing the global error pill
    /// (the K-Graph route has its own real estate for the gap).
    MemoryGraphResult(anyhow::Result<MemoryGraphResponse>),
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
    /// Result of a user-initiated Commands-launchpad run. The
    /// [`RunnableCommand`] variant identifies which row was clicked;
    /// the body is a Debug-formatted string of the response (no
    /// JSON pretty-print since most response types are
    /// `Deserialize`-only — promoting them to `Serialize` is a
    /// bigger refactor than this slice deserves).
    CommandRunResult(
        crate::routes::commands::RunnableCommand,
        Result<String, String>,
    ),
    Sse(SseEvent),
    /// One observation for the MCP inspector ring. Posted from
    /// background instrumentation (e.g. the sampling-offer
    /// handler's `SamplingObserver` impl) over the same flume
    /// channel everything else uses, so the gpui-side mutation
    /// (`AppState::record_inspector_event`) stays single-threaded.
    /// See `crate::inspector::InspectorSamplingObserver`.
    InspectorRecord(crate::inspector::CallEvent),
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
    /// Tool-call timeline / inspector — richer rendering of the same
    /// SSE activity stream the Home dashboard's activity tile shows,
    /// with per-tool colour coding, filters, pinning, and a right-rail
    /// inspector pane (#293).
    Timeline,
    /// Knowledge graph canvas — a second graph view (distinct from
    /// the Home dashboard's relations graph), backed by drawer
    /// cross-references. Stubbed today: a server-side
    /// `/api/memory/graph` endpoint is required and tracked
    /// separately. The route surfaces the gap inline so the slot
    /// reads as a known greenfield, not an empty tab.
    KnowledgeGraph,
    /// Embedded terminal — alacritty_terminal-backed shell session
    /// rendered in GPUI (issue #402). Each visit owns one shell for
    /// the route's lifetime; switching tabs and back keeps the
    /// session alive.
    Terminal,
    /// MCP call inspector — two-pane waterfall over every MCP
    /// message (internal in-proc bridge + future external peers).
    /// Distinct from `Timeline` (which renders the SSE activity
    /// stream); this one is the meta layer. Spec at
    /// `crates/crabcc-desktop/docs/MCP-INSPECTOR.md`. Eventually
    /// supersedes `System` per `MCP-NATIVE.md` §2; for M0 ships
    /// alongside it so adoption stays additive.
    Inspector,
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
            Route::Timeline => "Timeline",
            Route::KnowledgeGraph => "K-Graph",
            Route::Terminal => "Terminal",
            Route::Inspector => "Inspector",
        }
    }

    /// Window-title string for this route. Static per route so
    /// `Shell::render`'s last-title sentinel can be a cheap
    /// `Option<&'static str>` equality check, with no per-frame
    /// `format!()` allocation.
    pub fn window_title(self) -> &'static str {
        match self {
            Route::Home => "crabcc · live · Home",
            Route::Agents => "crabcc · live · Agents",
            Route::Logs => "crabcc · live · Logs",
            Route::System => "crabcc · live · System",
            Route::Knowledge => "crabcc · live · Knowledge",
            Route::Commands => "crabcc · live · Commands",
            Route::Timeline => "crabcc · live · Timeline",
            Route::KnowledgeGraph => "crabcc · live · K-Graph",
            Route::Terminal => "crabcc · live · Terminal",
            Route::Inspector => "crabcc · live · Inspector",
        }
    }

    pub const ALL: [Route; 10] = [
        Route::Home,
        Route::Agents,
        Route::Logs,
        Route::System,
        Route::Knowledge,
        Route::Commands,
        Route::Timeline,
        Route::Inspector,
        Route::KnowledgeGraph,
        Route::Terminal,
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
    /// Drawer cross-reference graph from `/api/memory/graph`. Fetched
    /// lazily by the K-Graph route via [`AppState::submit_memory_graph`]
    /// — no periodic refresh, since the drawer set churns slower than
    /// activity / telemetry. Replaced wholesale on success; `None`
    /// until the route triggers the first fetch (#317).
    pub memory_graph: Option<MemoryGraphResponse>,
    /// Most recent error (if any) from the last `memory_graph` fetch.
    /// Distinct from `last_error` so the K-Graph route can surface
    /// the gap inline without competing for the global error pill.
    pub memory_graph_error: Option<String>,
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
    /// Agent id assigned by the server on the most recent successful
    /// launch. Captured separately from `last_launch` (which is a
    /// human-facing status string) so the agent-spawn sheet can pick
    /// up the id and transition into its streaming view. `None` until
    /// a launch succeeds; cleared on `submit_launch` so the sheet
    /// doesn't replay a stale id on the next attempt.
    pub last_launch_id: Option<SharedString>,
    /// Mirror of `last_ingest` for the per-agent kill button.
    pub last_kill: Option<Result<String, String>>,
    /// Most recent per-agent log fetch (Agents route). Holds both the
    /// selected agent id and the result body so the view can render a
    /// "stale" warning if the id no longer matches the active selection.
    /// Cleared explicitly when the user deselects.
    pub agent_log: Option<AgentLogState>,
    /// Currently-running Commands-launchpad row, if any. Used to pulse
    /// a "running…" affordance on the matching row. Set by
    /// `submit_command_run`; cleared when the matching
    /// `CommandRunResult` lands.
    pub running_command: Option<crate::routes::commands::RunnableCommand>,
    /// Most recent Commands-launchpad result. Single-slot — a fresh
    /// click clears it before submitting the next run.
    pub last_command_run: Option<(
        crate::routes::commands::RunnableCommand,
        Result<String, String>,
    )>,
    /// Currently selected route — driven by the header nav clicks. The
    /// shell view re-renders on `cx.notify` and dispatches body content
    /// based on this value.
    pub route: Route,
    /// Staging slot for a one-shot agent_id pin that should land on
    /// the Timeline route's own `agent_pin` field the next time it
    /// renders. Set by `navigate_to_timeline_with_agent_pin` (e.g.
    /// the dashboard's "→ Timeline" affordance); the Timeline route
    /// reads-and-clears it via `take_pending_timeline_agent_pin` so
    /// the pin survives exactly one navigation handoff.
    pub pending_timeline_agent_pin: Option<SharedString>,
    /// Staging slot for a one-shot op-name pin on the Timeline route.
    /// Companion to `pending_timeline_agent_pin` covering the other
    /// dashboard pin axis. Set by `navigate_to_timeline_with_op_pin`
    /// (the dashboard's op-pin pill's "→ Timeline" sibling); Timeline's
    /// `Render` mirrors it into `op_pin` once.
    pub pending_timeline_op_pin: Option<SharedString>,
    /// Staging slot for a one-shot Agents-route selection. Set by
    /// `navigate_to_agents_with_selection` (e.g. the Timeline's
    /// "→ Agents" affordance); the Agents route reads-and-clears it
    /// via `take_pending_agents_selected_id` and pre-expands the
    /// matching row's log-tail panel without an extra click.
    pub pending_agents_selected_id: Option<SharedString>,
    /// Staging slot for a one-shot Knowledge-route filter string.
    /// Set by `navigate_to_knowledge_with_filter` (e.g. the K-Graph's
    /// "→ Knowledge" affordance from the node-detail panel); the
    /// Knowledge route reads-and-clears it via
    /// `take_pending_knowledge_filter`, mirroring it into both the
    /// `filter_lower` field and the `InputState` text so the input
    /// shows what's actually narrowing the list.
    pub pending_knowledge_filter: Option<SharedString>,
    /// Staging slot for a one-shot K-Graph node selection. Set by
    /// `navigate_to_kgraph_with_selection` (e.g. the Knowledge route's
    /// per-drawer "→ K-Graph" affordance); the K-Graph route reads-
    /// and-clears it via `take_pending_kgraph_selected_id` and pre-
    /// selects the matching pill so its detail + neighbour highlight
    /// land on entry.
    pub pending_kgraph_selected_id: Option<SharedString>,
    /// Staging slot for a one-shot agent-spawn flow: profile id to
    /// pre-select when the dashboard's spawn sheet opens. Set by
    /// `navigate_to_dashboard_with_spawn_profile` (e.g. the System
    /// route's per-profile "Launch agent" affordance); the dashboard
    /// reads-and-clears it via `take_pending_spawn_profile` on its
    /// next render.
    pub pending_spawn_profile: Option<SharedString>,
    /// Staging slot for a one-shot Knowledge-route wing-pin. Set by
    /// `navigate_to_knowledge_with_wing_pin` (e.g. the K-Graph's
    /// wing-list section header click); the Knowledge route reads-
    /// and-clears it via `take_pending_knowledge_wing_pin` and
    /// applies it to its `wing_pin` field on next render. Distinct
    /// from `pending_knowledge_filter` (substring filter); both can
    /// stage independently.
    pub pending_knowledge_wing_pin: Option<SharedString>,
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
    /// Whether visible toasts should ALSO be echoed to the macOS
    /// Notification Center via `native::deliver_notification` (track
    /// C.2). Defaults to `true` — every visible toast fires a system
    /// banner. Toggle off via the header `↗ system` button for
    /// "in-window only" mode (e.g. operator on screen-sharing call
    /// who doesn't want banners cluttering the recording).
    ///
    /// `Shell::render`'s delivery hook still advances its
    /// last-delivered-id sentinel even when echo is off, so toggling
    /// echo back on doesn't blast the queued-but-suppressed toasts.
    pub echo_to_system: bool,
    /// Append-only log of every toast that has been emitted, even
    /// when muted. Capped at [`TOAST_HISTORY_CAP`] entries — over-cap
    /// pushes drop the oldest from the front. Drives the
    /// "Show last N →" view (track C.0 slice 5+) so users can audit
    /// notifications they missed (or muted away).
    pub toast_history: VecDeque<Toast>,
    /// Index into [`crate::theme::Palette::ALL_NAMES`] — current
    /// theme palette. The header palette-cycle button advances
    /// this; `Shell::render` reads the name to label the button.
    /// Initial value comes from `CRABCC_DESKTOP_PALETTE` if set,
    /// otherwise the OS-appearance default (resolved via
    /// `theme::initial_palette_index`).
    pub palette_index: usize,
    /// MCP-message ring buffer feeding the Inspector route. Bounded
    /// by [`INSPECTOR_RING_CAP`]; over-cap pushes evict the oldest.
    /// Populated via [`AppState::record_inspector_event`] — for M0
    /// the in-proc MCP bridge isn't yet wired, so callers are
    /// purely opt-in. See
    /// `crates/crabcc-desktop/docs/MCP-INSPECTOR.md` for the design.
    pub inspector_ring: VecDeque<CallEvent>,
    /// MCP server lifecycle handle. `Some` when the desktop bound
    /// a Unix socket and is listening for inbound MCP requests
    /// (`sampling/createMessage` from `BullmqRuntime` containers,
    /// the iPhone bridge, etc.). `None` when startup failed —
    /// usually because `LITELLM_MASTER_KEY` is unset on this host.
    /// Drop unlinks the socket file. See `crate::mcp_server`.
    #[allow(dead_code)]
    pub mcp_server: Option<crate::mcp_server::McpServerHandle>,
    /// Resource snapshot shared with the
    /// `AppStateResourceProvider` wired into the sampling handler.
    /// Refreshed on every `apply()` (excluding inspector events).
    /// `None` when no MCP server is running — saves the per-event
    /// refresh cost when there's no consumer. See
    /// `crate::resources`.
    pub resource_snapshot: Option<Arc<RwLock<crate::resources::ResourceSnapshot>>>,
}

/// Cap on the toast history log. The brief calls out
/// "Show last 50 →" — pinning that here so the cap is one place to
/// edit when we revisit.
pub const TOAST_HISTORY_CAP: usize = 50;

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
        // `Default` gives `echo_to_system: false` (bool defaults to
        // false) but we want the system-echo on by default — every
        // visible toast also fires a banner unless the user opts
        // out. Override here so the field doesn't need a manual
        // `Default` impl on the whole struct.
        //
        // `palette_index` likewise needs to honour the
        // `CRABCC_DESKTOP_PALETTE` env var so the header switcher
        // button matches what `theme::install` actually applied.
        Self {
            echo_to_system: true,
            palette_index: crate::theme::initial_palette_index(),
            ..Self::default()
        }
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
        // Wall-clock proxy: prefer the latest observed event ts so
        // the timestamp matches other UI surfaces (KPI strip "X
        // seconds ago"). Falls back to 0 before the first event.
        let created_at = self.last_event_ts.unwrap_or(0);
        let toast = Toast {
            id,
            level,
            message: message.into(),
            created_at,
        };
        // Always log to history — even muted toasts are recorded so
        // the operator can audit what was suppressed via the
        // "Show last N" view.
        if self.toast_history.len() >= TOAST_HISTORY_CAP {
            self.toast_history.pop_front();
        }
        self.toast_history.push_back(toast.clone());
        if self.toasts_muted {
            // Sentinel-only: edge-trigger code (slice 3) needs a
            // unique id even when muted, so dismiss-on-recover
            // doesn't accidentally target somebody else's toast.
            return id;
        }
        self.gc_expired_toasts();
        if self.toasts.len() >= MAX_VISIBLE_TOASTS {
            self.toasts.pop_back();
        }
        self.toasts.push_front(toast);
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

    /// Flip the system-echo state. No deque side-effects — the
    /// in-window strip is unchanged either way; the next toast
    /// pushed will be (or won't be) echoed to Notification Center
    /// per the new value. See [`Shell::render`]'s delivery hook
    /// for the consumer side.
    pub fn toggle_echo_to_system(&mut self) {
        self.echo_to_system = !self.echo_to_system;
    }

    /// Advance to the next theme palette in
    /// [`crate::theme::Palette::ALL_NAMES`], wrapping at the end.
    /// The caller is responsible for actually applying the new
    /// palette (`theme::apply_by_index`) and forcing a re-render
    /// (`window.refresh()`) — this method just bumps the index so
    /// other components that read the name (header label) reflect
    /// the change in their next render.
    pub fn cycle_palette(&mut self) {
        let len = crate::theme::Palette::ALL_NAMES.len();
        self.palette_index = (self.palette_index + 1) % len;
    }

    /// Lookup-friendly accessor — returns the current palette's
    /// canonical name. Slot-safe: out-of-range indexes wrap to 0.
    pub fn palette_name(&self) -> &'static str {
        let names = crate::theme::Palette::ALL_NAMES;
        names[self.palette_index % names.len()]
    }

    /// Wipe the toast history log. Doesn't touch the visible deque
    /// — those are independent surfaces (active vs audit log).
    pub fn clear_toast_history(&mut self) {
        self.toast_history.clear();
    }

    /// Wipe all currently-visible toasts. Doesn't touch the
    /// history log (the audit trail survives a UI dismiss-all),
    /// and doesn't reset edge-trigger sentinels — if the
    /// telemetry warning was visible and gets dismissed, the
    /// next failure cycle is treated as a fresh first-failure
    /// and re-emits a Warning. That's the right behaviour: the
    /// user clicked "all gone", but the underlying conditions
    /// are still tracked.
    pub fn clear_visible_toasts(&mut self) {
        self.toasts.clear();
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
            // Inspector observations go straight into the ring.
            // No notify-bookkeeping or telemetry side effects —
            // the route observes `AppState` and re-renders on the
            // outer `cx.notify()` in `pump_events`.
            AppEvent::InspectorRecord(call_event) => {
                self.record_inspector_event(call_event);
                return;
            }
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
                // Detect Running → Exited transitions and pop one
                // Info toast per. Skip when the previous list was
                // empty (first frame after startup) — otherwise the
                // bootstrap "all already-Exited agents" history would
                // toast each one. The system catalog of exits stays
                // available via the Agents route's persistent list
                // and the toast history log.
                if !self.agents.is_empty() {
                    let prev_running: std::collections::HashSet<SharedString> = self
                        .agents
                        .iter()
                        .filter(|a| matches!(a.status, AgentStatus::Running))
                        .map(|a| a.id.clone())
                        .collect();
                    let exits: Vec<SharedString> = frame
                        .agents
                        .iter()
                        .filter(|a| {
                            matches!(a.status, AgentStatus::Exited) && prev_running.contains(&a.id)
                        })
                        .map(|a| a.id.clone())
                        .collect();
                    for id in exits {
                        self.push_toast(ToastLevel::Info, format!("agent {id} exited"));
                    }
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
            AppEvent::MemoryGraphResult(Ok(snapshot)) => {
                self.memory_graph = Some(snapshot);
                self.memory_graph_error = None;
            }
            AppEvent::MemoryGraphResult(Err(e)) => {
                self.memory_graph_error = Some(format!("memory_graph: {e}"));
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
                // Capture the agent id for the spawn sheet's
                // Launching → Streaming transition. The toast above
                // is for the global notification stripe; this slot
                // is the durable handle the sheet observes.
                if let Some(id) = resp.id.as_ref() {
                    self.last_launch_id = Some(id.clone().into());
                }
            }
            AppEvent::AgentLaunchResult(Err(e)) => {
                let msg = format!("launch failed: {e}");
                self.push_toast(ToastLevel::Danger, msg.clone());
                self.last_launch = Some(Err(msg));
                self.last_launch_id = None;
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
            AppEvent::CommandRunResult(cmd, result) => {
                if self.running_command == Some(cmd) {
                    self.running_command = None;
                }
                self.last_command_run = Some((cmd, result));
            }
        }
        // Keep the resource snapshot fresh for the MCP server's
        // includeContext flow. Cheap (string assembly only) and
        // bounded by the per-section caps in `crate::resources`.
        // InspectorRecord events early-return above so they don't
        // pay this cost — they're meta and don't affect the
        // resource view anyway.
        self.refresh_resource_snapshot();
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

    /// Fire a one-shot `/api/memory/graph` fetch. Result replaces
    /// `memory_graph` on success; `Err` lands in `memory_graph_error`.
    /// Triggered by the K-Graph route on first render + on a manual
    /// refresh button (#317).
    pub fn submit_memory_graph(&self) {
        let Some(handles) = self.workers.clone() else {
            return;
        };
        let WorkerHandles { tx, base_url } = handles;
        std::thread::Builder::new()
            .name("crabcc-memory-graph".into())
            .spawn(move || {
                debug!(target: "crabcc::state", thread = "memory-graph", "starting");
                let client = Client::with_base_url(base_url);
                let result = client.memory_graph();
                let _ = try_send_app_event(&tx, AppEvent::MemoryGraphResult(result));
            })
            .expect("memory-graph thread spawn");
    }

    /// Fire a one-shot `/api/memory/recent` fetch. Same wire shape as
    /// the 10s polling tick (which also produces `MemoryRefresh`),
    /// just kicked off on demand. Used by the Knowledge route's
    /// manual refresh button so the user doesn't have to wait for
    /// the next poll after a CLI ingest landed elsewhere.
    pub fn submit_memory_refresh(&self) {
        let Some(handles) = self.workers.clone() else {
            return;
        };
        let WorkerHandles { tx, base_url } = handles;
        std::thread::Builder::new()
            .name("crabcc-memory-refresh".into())
            .spawn(move || {
                debug!(target: "crabcc::state", thread = "memory-refresh", "starting");
                let client = Client::with_base_url(base_url);
                let result = client.memory_recent();
                let _ = try_send_app_event(&tx, AppEvent::MemoryRefresh(result));
            })
            .expect("memory-refresh thread spawn");
    }

    /// Fire a Commands-launchpad row's HTTP method on a detached
    /// thread. Mutates `running_command` + clears `last_command_run`
    /// up front so the calling row can pulse a "running…" indicator
    /// before the worker channel round-trips. Result is JSON-pretty
    /// formatted by `run_command` (#314 + #315).
    pub fn submit_command_run(&mut self, cmd: crate::routes::commands::RunnableCommand) {
        self.running_command = Some(cmd);
        self.last_command_run = None;
        let Some(handles) = self.workers.clone() else {
            return;
        };
        let WorkerHandles { tx, base_url } = handles;
        std::thread::Builder::new()
            .name("crabcc-command-run".into())
            .spawn(move || {
                debug!(
                    target: "crabcc::state",
                    thread = "command-run",
                    command = ?cmd,
                    "starting"
                );
                let client = Client::with_base_url(base_url);
                let result = run_command(&client, cmd, &tx).map_err(|e| e.to_string());
                let _ = try_send_app_event(&tx, AppEvent::CommandRunResult(cmd, result));
            })
            .expect("command-run thread spawn");
    }

    /// Rebuild the [`crate::resources::ResourceSnapshot`] from
    /// current state. No-op when no snapshot is wired (i.e. no
    /// MCP server running). Cheap — string assembly only,
    /// microseconds per call.
    pub fn refresh_resource_snapshot(&self) {
        let Some(snap) = self.resource_snapshot.as_ref() else {
            return;
        };
        let new = crate::resources::ResourceSnapshot::from_state(self);
        if let Ok(mut w) = snap.write() {
            *w = new;
        }
        // Poisoned lock = silently skip; instrumentation must
        // never break the call path.
    }

    /// Append one MCP-call observation to the inspector ring.
    /// Evicts the oldest entry once the ring is full so the buffer
    /// stays bounded at [`INSPECTOR_RING_CAP`]. Callers are anywhere
    /// that handles an MCP message; for M0 nothing inside the
    /// crate calls this yet — the in-proc bridge wiring lands in M1.
    pub fn record_inspector_event(&mut self, evt: CallEvent) {
        if self.inspector_ring.len() >= INSPECTOR_RING_CAP {
            self.inspector_ring.pop_front();
        }
        self.inspector_ring.push_back(evt);
    }

    pub fn set_route(&mut self, route: Route) {
        self.route = route;
    }

    /// Navigate to the Timeline route and stage `agent_id` so the
    /// Timeline route applies it as its `agent_pin` on next render.
    /// Used by the dashboard's "→ Timeline" cross-link to dive from
    /// the small activity tile into the richer per-agent view.
    pub fn navigate_to_timeline_with_agent_pin(&mut self, agent_id: SharedString) {
        self.pending_timeline_agent_pin = Some(agent_id);
        self.route = Route::Timeline;
    }

    /// Read-and-clear the pending agent pin. Idempotent across renders
    /// — once consumed by the Timeline route, subsequent renders see
    /// `None` and don't keep re-applying the pin.
    pub fn take_pending_timeline_agent_pin(&mut self) -> Option<SharedString> {
        self.pending_timeline_agent_pin.take()
    }

    /// Navigate to the Timeline route and stage `op` so the route's
    /// `op_pin` activates on next render. Companion to the agent-pin
    /// handoff for the other axis the dashboard activity tile pins on.
    pub fn navigate_to_timeline_with_op_pin(&mut self, op: SharedString) {
        self.pending_timeline_op_pin = Some(op);
        self.route = Route::Timeline;
    }

    /// Read-and-clear the pending op pin. One-shot, same shape as
    /// `take_pending_timeline_agent_pin`.
    pub fn take_pending_timeline_op_pin(&mut self) -> Option<SharedString> {
        self.pending_timeline_op_pin.take()
    }

    /// Navigate to the Agents route and stage `agent_id` so the route
    /// pre-selects that row (expanding its log-tail panel) on next
    /// render. Used by the Timeline's "→ Agents" cross-link.
    pub fn navigate_to_agents_with_selection(&mut self, agent_id: SharedString) {
        self.pending_agents_selected_id = Some(agent_id);
        self.route = Route::Agents;
    }

    /// Read-and-clear the pending Agents-route selection. Same one-
    /// shot semantics as `take_pending_timeline_agent_pin` — applied
    /// once and then cleared so subsequent renders don't keep
    /// overriding manual deselection.
    pub fn take_pending_agents_selected_id(&mut self) -> Option<SharedString> {
        self.pending_agents_selected_id.take()
    }

    /// Navigate to the Knowledge route and stage `filter` so the
    /// route's filter input lands pre-populated. Used by the K-Graph's
    /// "→ Knowledge" cross-link to dive from a selected canvas node
    /// into its drawer view.
    pub fn navigate_to_knowledge_with_filter(&mut self, filter: SharedString) {
        self.pending_knowledge_filter = Some(filter);
        self.route = Route::Knowledge;
    }

    /// Read-and-clear the pending Knowledge filter. One-shot — once
    /// the Knowledge route's render has mirrored it into the
    /// InputState + `filter_lower`, the slot stays empty until staged
    /// again.
    pub fn take_pending_knowledge_filter(&mut self) -> Option<SharedString> {
        self.pending_knowledge_filter.take()
    }

    /// Navigate to the K-Graph route and stage `node_id` so the route
    /// pre-selects that pill on render. Used by Knowledge's per-row
    /// "→ K-Graph" cross-link to dive from a drawer into its canvas
    /// neighbourhood.
    pub fn navigate_to_kgraph_with_selection(&mut self, node_id: SharedString) {
        self.pending_kgraph_selected_id = Some(node_id);
        self.route = Route::KnowledgeGraph;
    }

    /// Read-and-clear the pending K-Graph selection. One-shot — the
    /// K-Graph render applies it once and the slot stays empty until
    /// staged again.
    pub fn take_pending_kgraph_selected_id(&mut self) -> Option<SharedString> {
        self.pending_kgraph_selected_id.take()
    }

    /// Navigate to Home and stage `profile_id` so the dashboard's
    /// spawn sheet opens pre-populated with that profile selected.
    /// Used by the System route's per-profile launch affordance.
    pub fn navigate_to_dashboard_with_spawn_profile(&mut self, profile_id: SharedString) {
        self.pending_spawn_profile = Some(profile_id);
        self.route = Route::Home;
    }

    /// Read-and-clear the pending spawn-profile id. One-shot —
    /// dashboard applies it once on next render and the slot stays
    /// empty until staged again. Closing or submitting the sheet
    /// won't re-trigger the staged profile.
    pub fn take_pending_spawn_profile(&mut self) -> Option<SharedString> {
        self.pending_spawn_profile.take()
    }

    /// Navigate to the Knowledge route and stage `wing` so the
    /// route's `wing_pin` field activates on next render. Used by
    /// the K-Graph's wing-list section-header click to dive from a
    /// canvas wing group to its drawer body view.
    pub fn navigate_to_knowledge_with_wing_pin(&mut self, wing: SharedString) {
        self.pending_knowledge_wing_pin = Some(wing);
        self.route = Route::Knowledge;
    }

    /// Read-and-clear the pending Knowledge wing-pin. One-shot,
    /// same shape as `take_pending_knowledge_filter`.
    pub fn take_pending_knowledge_wing_pin(&mut self) -> Option<SharedString> {
        self.pending_knowledge_wing_pin.take()
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
    fn push_logs_to_history_even_when_muted() {
        // History is append-only and records every push, including
        // muted ones — so the operator can audit what was suppressed.
        let mut s = AppState::new();
        s.toasts_muted = true;
        s.push_toast(ToastLevel::Info, "muted-1");
        s.push_toast(ToastLevel::Warning, "muted-2");
        // Visible deque stays empty (mute), but history captures both.
        assert!(s.toasts.is_empty());
        assert_eq!(s.toast_history.len(), 2);
        assert_eq!(s.toast_history.front().unwrap().message, "muted-1");
        assert_eq!(s.toast_history.back().unwrap().message, "muted-2");
    }

    #[test]
    fn history_caps_at_50_dropping_oldest() {
        // Push 60 → keep the newest 50, oldest 10 are evicted from
        // the front. Newest at the back, oldest at the front.
        let mut s = AppState::new();
        for n in 0..60 {
            s.push_toast(ToastLevel::Info, format!("n={n}"));
        }
        assert_eq!(s.toast_history.len(), TOAST_HISTORY_CAP);
        assert_eq!(s.toast_history.front().unwrap().message, "n=10");
        assert_eq!(s.toast_history.back().unwrap().message, "n=59");
    }

    #[test]
    fn clear_history_leaves_visible_alone() {
        // Active deque and history are independent surfaces — clear
        // history must not touch the visible toasts.
        let mut s = AppState::new();
        s.push_toast(ToastLevel::Info, "live");
        assert_eq!(s.toasts.len(), 1);
        assert_eq!(s.toast_history.len(), 1);
        s.clear_toast_history();
        assert_eq!(s.toasts.len(), 1, "active toast must survive clear_history");
        assert!(s.toast_history.is_empty());
    }

    #[test]
    fn clear_visible_leaves_history_alone() {
        // Visible deque and history are independent. Wiping
        // visible must not touch history (the audit trail
        // survives a UI dismiss-all).
        let mut s = AppState::new();
        s.push_toast(ToastLevel::Info, "a");
        s.push_toast(ToastLevel::Warning, "b");
        assert_eq!(s.toasts.len(), 2);
        assert_eq!(s.toast_history.len(), 2);
        s.clear_visible_toasts();
        assert!(s.toasts.is_empty());
        assert_eq!(s.toast_history.len(), 2, "history must survive");
    }

    #[test]
    fn echo_to_system_default_on_then_toggle_off() {
        // Default state: echo on. Toggle flips it; toggle again
        // restores. No deque side-effects either direction.
        let mut s = AppState::new();
        assert!(s.echo_to_system, "default must be on");
        s.push_toast(ToastLevel::Info, "x");
        let visible_before = s.toasts.len();
        s.toggle_echo_to_system();
        assert!(!s.echo_to_system);
        // Visible deque unchanged — echo toggle is purely about
        // the system-side delivery, not the in-window strip.
        assert_eq!(s.toasts.len(), visible_before);
        s.toggle_echo_to_system();
        assert!(s.echo_to_system);
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

    fn make_agent(id: &str, status: AgentStatus) -> SseAgent {
        SseAgent {
            id: id.into(),
            status,
            started_ts: 0,
            pid: None,
            runtime: None,
            model: None,
            prompt_preview: SharedString::default(),
            log_bytes: 0,
            root: None,
            derived: crate::api::types::AgentDerived::default(),
        }
    }

    #[test]
    fn agent_running_to_exited_transition_pops_info_toast() {
        // Establish the prior state (agent A running). Then a frame
        // arrives where A is Exited — pops one Info toast.
        let mut s = AppState::new();
        s.last_event_ts = Some(0);
        s.apply(AppEvent::Sse(SseEvent::Agents(
            crate::api::types::SseAgentsFrame {
                agents: vec![make_agent("a", AgentStatus::Running)],
            },
        )));
        // First frame seeds prior state — no toasts yet.
        assert!(s.toasts.is_empty());

        // Second frame: A has exited.
        s.apply(AppEvent::Sse(SseEvent::Agents(
            crate::api::types::SseAgentsFrame {
                agents: vec![make_agent("a", AgentStatus::Exited)],
            },
        )));
        assert_eq!(s.toasts.len(), 1);
        let t = s.toasts.front().unwrap();
        assert!(matches!(t.level, ToastLevel::Info));
        assert_eq!(t.message, "agent a exited");
    }

    #[test]
    fn first_agents_frame_with_only_exited_does_not_toast() {
        // Bootstrap case: the very first SSE Agents frame may carry
        // already-Exited agents from the server's history. Toasting
        // each one would spam the strip with stale events. Skip when
        // the prior `self.agents` was empty.
        let mut s = AppState::new();
        s.last_event_ts = Some(0);
        s.apply(AppEvent::Sse(SseEvent::Agents(
            crate::api::types::SseAgentsFrame {
                agents: vec![
                    make_agent("a", AgentStatus::Exited),
                    make_agent("b", AgentStatus::Exited),
                ],
            },
        )));
        assert!(s.toasts.is_empty(), "first frame must not pop toasts");
    }

    #[test]
    fn already_exited_agent_in_subsequent_frame_does_not_toast() {
        // Agent B was already Exited in the prior frame. Re-receiving
        // it as Exited must NOT pop a duplicate toast.
        let mut s = AppState::new();
        s.last_event_ts = Some(0);
        s.apply(AppEvent::Sse(SseEvent::Agents(
            crate::api::types::SseAgentsFrame {
                agents: vec![
                    make_agent("a", AgentStatus::Running),
                    make_agent("b", AgentStatus::Exited),
                ],
            },
        )));
        assert!(s.toasts.is_empty());
        s.apply(AppEvent::Sse(SseEvent::Agents(
            crate::api::types::SseAgentsFrame {
                agents: vec![
                    make_agent("a", AgentStatus::Running),
                    make_agent("b", AgentStatus::Exited),
                ],
            },
        )));
        assert!(s.toasts.is_empty(), "already-exited B must not re-toast");
    }

    #[test]
    fn navigate_to_timeline_with_agent_pin_sets_route_and_stages_pin() {
        let mut s = AppState::new();
        assert_eq!(s.route, Route::default());
        assert!(s.pending_timeline_agent_pin.is_none());
        s.navigate_to_timeline_with_agent_pin("agent-deadbeef".into());
        assert_eq!(s.route, Route::Timeline);
        assert_eq!(
            s.pending_timeline_agent_pin.as_deref(),
            Some("agent-deadbeef")
        );
    }

    #[test]
    fn take_pending_timeline_agent_pin_is_one_shot() {
        let mut s = AppState::new();
        s.navigate_to_timeline_with_agent_pin("a".into());
        assert_eq!(s.take_pending_timeline_agent_pin().as_deref(), Some("a"));
        // Second take must be None — the slot is one-shot, otherwise
        // every Timeline render would re-apply the pin and the user
        // could never clear it.
        assert_eq!(s.take_pending_timeline_agent_pin(), None);
    }

    #[test]
    fn navigate_to_timeline_with_op_pin_sets_route_and_stages_op() {
        let mut s = AppState::new();
        assert!(s.pending_timeline_op_pin.is_none());
        s.navigate_to_timeline_with_op_pin("sym".into());
        assert_eq!(s.route, Route::Timeline);
        assert_eq!(s.pending_timeline_op_pin.as_deref(), Some("sym"));
    }

    #[test]
    fn take_pending_timeline_op_pin_is_one_shot() {
        let mut s = AppState::new();
        s.navigate_to_timeline_with_op_pin("refs".into());
        assert_eq!(s.take_pending_timeline_op_pin().as_deref(), Some("refs"));
        assert_eq!(s.take_pending_timeline_op_pin(), None);
    }

    #[test]
    fn timeline_agent_and_op_pin_handoffs_are_independent() {
        // Both pins can be staged for the same Timeline render — the
        // dashboard only ever fires one or the other, but the slots
        // shouldn't interfere if a future entry point sets both.
        let mut s = AppState::new();
        s.navigate_to_timeline_with_agent_pin("a".into());
        s.navigate_to_timeline_with_op_pin("sym".into());
        assert_eq!(s.route, Route::Timeline);
        assert_eq!(s.pending_timeline_agent_pin.as_deref(), Some("a"));
        assert_eq!(s.pending_timeline_op_pin.as_deref(), Some("sym"));
    }

    #[test]
    fn navigate_to_agents_with_selection_sets_route_and_stages_id() {
        let mut s = AppState::new();
        assert!(s.pending_agents_selected_id.is_none());
        s.navigate_to_agents_with_selection("agent-deadbeef".into());
        assert_eq!(s.route, Route::Agents);
        assert_eq!(
            s.pending_agents_selected_id.as_deref(),
            Some("agent-deadbeef")
        );
    }

    #[test]
    fn take_pending_agents_selected_id_is_one_shot() {
        let mut s = AppState::new();
        s.navigate_to_agents_with_selection("a".into());
        assert_eq!(s.take_pending_agents_selected_id().as_deref(), Some("a"));
        assert_eq!(s.take_pending_agents_selected_id(), None);
    }

    #[test]
    fn timeline_and_agents_handoff_slots_are_independent() {
        // Setting one navigation handoff must not stomp the other —
        // both can coexist mid-render even though we never use them
        // together in the UI today (defends against future refactors).
        let mut s = AppState::new();
        s.navigate_to_timeline_with_agent_pin("a".into());
        s.navigate_to_agents_with_selection("b".into());
        // Latest call wins for `route`; the slots themselves are
        // independent.
        assert_eq!(s.route, Route::Agents);
        assert_eq!(s.pending_timeline_agent_pin.as_deref(), Some("a"));
        assert_eq!(s.pending_agents_selected_id.as_deref(), Some("b"));
    }

    #[test]
    fn navigate_to_knowledge_with_filter_sets_route_and_stages_filter() {
        let mut s = AppState::new();
        assert!(s.pending_knowledge_filter.is_none());
        s.navigate_to_knowledge_with_filter("doc:42".into());
        assert_eq!(s.route, Route::Knowledge);
        assert_eq!(s.pending_knowledge_filter.as_deref(), Some("doc:42"));
    }

    #[test]
    fn take_pending_knowledge_filter_is_one_shot() {
        let mut s = AppState::new();
        s.navigate_to_knowledge_with_filter("web:abc".into());
        assert_eq!(
            s.take_pending_knowledge_filter().as_deref(),
            Some("web:abc")
        );
        assert_eq!(s.take_pending_knowledge_filter(), None);
    }

    #[test]
    fn navigate_to_kgraph_with_selection_sets_route_and_stages_id() {
        let mut s = AppState::new();
        assert!(s.pending_kgraph_selected_id.is_none());
        s.navigate_to_kgraph_with_selection("doc:42".into());
        assert_eq!(s.route, Route::KnowledgeGraph);
        assert_eq!(s.pending_kgraph_selected_id.as_deref(), Some("doc:42"));
    }

    #[test]
    fn take_pending_kgraph_selected_id_is_one_shot() {
        let mut s = AppState::new();
        s.navigate_to_kgraph_with_selection("web:abc".into());
        assert_eq!(
            s.take_pending_kgraph_selected_id().as_deref(),
            Some("web:abc")
        );
        assert_eq!(s.take_pending_kgraph_selected_id(), None);
    }

    #[test]
    fn navigate_to_dashboard_with_spawn_profile_sets_route_and_stages_id() {
        let mut s = AppState::new();
        assert!(s.pending_spawn_profile.is_none());
        s.navigate_to_dashboard_with_spawn_profile("crabcc-default".into());
        assert_eq!(s.route, Route::Home);
        assert_eq!(s.pending_spawn_profile.as_deref(), Some("crabcc-default"));
    }

    #[test]
    fn take_pending_spawn_profile_is_one_shot() {
        let mut s = AppState::new();
        s.navigate_to_dashboard_with_spawn_profile("p1".into());
        assert_eq!(s.take_pending_spawn_profile().as_deref(), Some("p1"));
        assert_eq!(s.take_pending_spawn_profile(), None);
    }

    #[test]
    fn navigate_to_knowledge_with_wing_pin_sets_route_and_stages_wing() {
        let mut s = AppState::new();
        assert!(s.pending_knowledge_wing_pin.is_none());
        s.navigate_to_knowledge_with_wing_pin("doc".into());
        assert_eq!(s.route, Route::Knowledge);
        assert_eq!(s.pending_knowledge_wing_pin.as_deref(), Some("doc"));
    }

    #[test]
    fn take_pending_knowledge_wing_pin_is_one_shot() {
        let mut s = AppState::new();
        s.navigate_to_knowledge_with_wing_pin("session".into());
        assert_eq!(
            s.take_pending_knowledge_wing_pin().as_deref(),
            Some("session")
        );
        assert_eq!(s.take_pending_knowledge_wing_pin(), None);
    }

    #[test]
    fn knowledge_filter_and_wing_pin_handoffs_are_independent() {
        let mut s = AppState::new();
        s.navigate_to_knowledge_with_filter("foo".into());
        s.navigate_to_knowledge_with_wing_pin("doc".into());
        assert_eq!(s.route, Route::Knowledge);
        assert_eq!(s.pending_knowledge_filter.as_deref(), Some("foo"));
        assert_eq!(s.pending_knowledge_wing_pin.as_deref(), Some("doc"));
    }
}
