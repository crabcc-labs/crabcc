//! `AppState` — the shared model the dashboard view observes.
//!
//! Holds the latest snapshot of bootstrap, agents, services, telemetry,
//! and ring buffers for recent activity + telemetry events. Three
//! background workers feed it through a single `flume` channel:
//!
//! - `prefetch_worker` — one-shot at startup, GETs `/api/bootstrap`
//!   + `/api/services` + `/api/seed-graph` so the UI has something to
//!     draw before the first SSE frame arrives.
//! - `sse::spawn_worker` — long-lived, see `crate::sse`.
//! - `telemetry_worker` — periodic poll of `/api/telemetry?since=cursor`
//!   on a 3-second tick. Feeds the Logs route.
//!
//! The gpui side calls [`AppState::pump_events`] inside
//! `cx.spawn(async ...)` to drain the channel and update self via the
//! weak-entity pattern, calling `cx.notify()` after each update so
//! observers redraw.

use std::collections::VecDeque;
use std::time::Duration;

use gpui::{App, Context, Entity};

use crate::api::types::{
    Bootstrap, DiscoveryReport, GraphSnapshot, MemoryRecentResponse, SseActivityEvent, SseAgent,
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

/// One end-to-end app event. The workers ([`spawn_workers`]) multiplex
/// through this single channel so the gpui pump only needs one drain
/// loop.
#[derive(Debug)]
pub enum AppEvent {
    /// One-shot prefetch result. Either field may be `Err` independently
    /// — a dead service-discovery probe shouldn't withhold the bootstrap.
    Initial {
        bootstrap: anyhow::Result<Bootstrap>,
        services: anyhow::Result<DiscoveryReport>,
        graph: anyhow::Result<GraphSnapshot>,
        memory_recent: anyhow::Result<MemoryRecentResponse>,
    },
    /// Periodic telemetry poll. Carries new events since the last cursor
    /// plus the new cursor; `Err` skips this tick without resetting the
    /// cursor.
    Telemetry(anyhow::Result<TelemetrySnapshot>),
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
}

impl Route {
    pub fn label(self) -> &'static str {
        match self {
            Route::Home => "Home",
            Route::Logs => "Logs",
            Route::System => "System",
            Route::Knowledge => "Knowledge",
        }
    }

    pub const ALL: [Route; 4] = [Route::Home, Route::Logs, Route::System, Route::Knowledge];
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
    /// Currently selected route — driven by the header nav clicks. The
    /// shell view re-renders on `cx.notify` and dispatches body content
    /// based on this value.
    pub route: Route,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one event. Pure mutation — the caller wraps in
    /// `entity.update(cx, |this, cx| { this.apply(evt); cx.notify(); })`.
    pub fn apply(&mut self, evt: AppEvent) {
        match evt {
            AppEvent::Initial {
                bootstrap,
                services,
                graph,
                memory_recent,
            } => {
                match bootstrap {
                    Ok(b) => self.bootstrap = Some(b),
                    Err(e) => self.last_error = Some(format!("bootstrap: {e}")),
                }
                match services {
                    Ok(s) => self.services = Some(s),
                    Err(e) => self.last_error = Some(format!("services: {e}")),
                }
                match graph {
                    Ok(g) => self.graph = Some(g),
                    Err(e) => self.last_error = Some(format!("graph: {e}")),
                }
                match memory_recent {
                    Ok(m) => self.memory_recent = Some(m),
                    Err(e) => self.last_error = Some(format!("memory_recent: {e}")),
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
        }
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

/// Spawn the three background workers and return the receiving end
/// of the merged channel. Callers feed it into [`AppState::pump_events`].
///
/// Workers run on their own OS threads — no async runtime needed, and
/// [`flume::Receiver::recv_async`] works inside gpui's smol-flavored
/// `cx.spawn`.
pub fn spawn_workers(base_url: &str) -> flume::Receiver<AppEvent> {
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
                let bootstrap = client.bootstrap();
                let services = client.services();
                let graph = client.seed_graph();
                let memory_recent = client.memory_recent();
                // Receiver disconnect is fine — app shutdown raced us.
                let _ = tx.send(AppEvent::Initial {
                    bootstrap,
                    services,
                    graph,
                    memory_recent,
                });
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

    drop(tx);
    rx
}

/// Returns the AppState entity wired up with workers. Call from inside
/// a gpui context (e.g. `cx.new(|cx| build(cx, base))`).
pub fn build(cx: &mut Context<AppState>, base_url: &str) -> AppState {
    let rx = spawn_workers(base_url);
    let entity = cx.entity();
    AppState::pump_events(&entity, rx, cx);
    AppState::new()
}

// Suppress dead-code lint for the unused `_app` arg until A.5 needs it.
#[allow(dead_code)]
fn _app_marker(_app: &App) {}
