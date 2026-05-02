//! `AppState` — the shared model the dashboard view observes.
//!
//! Holds the latest snapshot of bootstrap, agents, services, and a
//! ring buffer of recent activity events. Two background workers feed
//! it through a single `flume` channel:
//!
//! - `prefetch_worker` — one-shot at startup, GETs `/api/bootstrap`
//!   + `/api/services` so the UI has something to draw before the
//!     first SSE frame arrives.
//! - `sse::spawn_worker` — long-lived, see `crate::sse`.
//!
//! The gpui side calls [`AppState::pump_events`] inside
//! `cx.spawn(async ...)` to drain the channel and update self via the
//! weak-entity pattern, calling `cx.notify()` after each update so
//! observers redraw.

use std::collections::VecDeque;

use gpui::{App, Context, Entity};

use crate::api::types::{Bootstrap, DiscoveryReport, SseActivityEvent, SseAgent};
use crate::api::Client;
use crate::sse::{self, SseEvent};

/// Cap on the in-memory recent-activity buffer. Tuned for the
/// DashboardHome tile (~5 visible rows + headroom). Large queries
/// that arrive in a single SSE frame still all land here, then age
/// off as new frames arrive.
const ACTIVITY_BUFFER: usize = 64;

/// One end-to-end app event. The two workers ([`spawn_workers`])
/// multiplex through this single channel so the gpui pump only needs
/// one drain loop.
#[derive(Debug)]
pub enum AppEvent {
    /// One-shot prefetch result. Either field may be `Err` independently
    /// — a dead service-discovery probe shouldn't withhold the bootstrap.
    Initial {
        bootstrap: anyhow::Result<Bootstrap>,
        services: anyhow::Result<DiscoveryReport>,
    },
    Sse(SseEvent),
}

#[derive(Default, Debug)]
pub struct AppState {
    pub bootstrap: Option<Bootstrap>,
    pub services: Option<DiscoveryReport>,
    pub agents: Vec<SseAgent>,
    pub recent_activity: VecDeque<SseActivityEvent>,
    /// Cumulative number of activity events seen since startup —
    /// not the number of rows in `recent_activity` (that's bounded).
    pub activity_total: u64,
    /// Last unix-seconds timestamp we observed in any incoming event.
    /// Drives the "X seconds ago" hint on the KPI strip.
    pub last_event_ts: Option<i64>,
    /// Most recent error from either worker (string-ified for display).
    pub last_error: Option<String>,
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
            } => {
                match bootstrap {
                    Ok(b) => self.bootstrap = Some(b),
                    Err(e) => self.last_error = Some(format!("bootstrap: {e}")),
                }
                match services {
                    Ok(s) => self.services = Some(s),
                    Err(e) => self.last_error = Some(format!("services: {e}")),
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
        }
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

/// Spawn the two background workers and return the receiving end of
/// the merged channel. Callers feed it into [`AppState::pump_events`].
///
/// Workers run on their own OS threads — no async runtime needed, and
/// [`flume::Receiver::recv_async`] works inside gpui's smol-flavored
/// `cx.spawn`.
pub fn spawn_workers(base_url: &str) -> flume::Receiver<AppEvent> {
    let (tx, rx) = flume::unbounded::<AppEvent>();

    // One-shot prefetch.
    {
        let tx = tx.clone();
        let base = base_url.to_string();
        std::thread::Builder::new()
            .name("crabcc-prefetch".into())
            .spawn(move || {
                let client = Client::with_base_url(base);
                let bootstrap = client.bootstrap();
                let services = client.services();
                // Receiver disconnect is fine — app shutdown raced us.
                let _ = tx.send(AppEvent::Initial {
                    bootstrap,
                    services,
                });
            })
            .expect("prefetch thread spawn");
    }

    // Long-lived SSE pump. Wrap each `SseEvent` in `AppEvent::Sse` on
    // its way out so `AppState::apply` only has one match arm shape.
    let sse_rx = sse::spawn_worker(base_url);
    {
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
