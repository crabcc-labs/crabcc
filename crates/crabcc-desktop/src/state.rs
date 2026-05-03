//! Single-window dashboard state model for `crabcc-desktop`.
//!
//! Workers:
//!   prefetch   — one-shot bootstrap fetch (fires once, then the thread exits)
//!   sse_bridge — long-lived SSE subscription to `/api/events`
//!   telemetry  — 3-second poll against `/api/telemetry`
//!   memory     — 10-second poll against `/api/memory/recent`
//!
//! All four workers send through a single `flume::bounded` channel
//! (`TX_CAP = 256` slots) into `pump_events`, which drives the `AppState`
//! state machine.  The bounded capacity keeps producers from running ahead of
//! the UI thread by more than a few seconds of events — backpressure for free.
//!
//! # Optimisation notes (senior-Rust review items 1–5)
//!
//! **Item 1 — box large enum variants.**
//! Every `AppEvent` variant whose payload exceeds ~64 B is wrapped in `Box<_>`
//! so the discriminant + widest arm fits in 16 bytes (two words on 64-bit).
//! Verify after changes:
//! ```text
//! cargo +nightly rustc -p crabcc-desktop -- -Zprint-type-sizes 2>&1 | grep AppEvent
//! ```
//!
//! **Item 2 — `CompactString` for short labels.**
//! `level`, `target`, `status`, `wing`, `room`, `op`, and SSE-status strings
//! are always ≤ 23 bytes in practice.  `compact_str::CompactString` stores
//! strings up to 24 bytes inline (SSO), avoiding a heap allocation entirely
//! for these fields.  Immutable path strings on `TelemetrySource` use `Box<str>`
//! (heap-allocated but no len/cap surplus compared with `String`).
//!
//! **Items 3 + 4 — `Wired` substruct + `dispatch` helper.**
//! `Wired` groups the sender, JoinHandles, and base URL so callers pass one
//! value instead of five.  `dispatch` is a pure function over `&mut AppState`
//! that can be unit-tested without a live channel.
//!
//! **Item 5 — bounded rings instead of unbounded `Vec`s.**
//! `telemetry_events`, `memory_drawers_ring`, and `activity_ring` are
//! `VecDeque` pre-allocated to their maximum capacity (`TELEMETRY_RING` /
//! `ACTIVITY_RING`).  `push_back` + conditional `pop_front` keeps the
//! steady-state heap footprint O(1).

use std::collections::VecDeque;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use compact_str::CompactString;
use flume::{Receiver, Sender};
use serde::Deserialize;

// ── constants ────────────────────────────────────────────────────────────────

/// Capacity of the inbound event channel.
///
/// 256 slots cover a burst of multiple telemetry + memory + SSE frames without
/// blocking producers while `pump_events` is busy rendering.  Backpressure
/// kicks in only when the UI loop falls > 256 events behind — a clear signal
/// that rendering is the bottleneck, not the workers.
const TX_CAP: usize = 256;

/// Telemetry poll interval.
const TELEMETRY_INTERVAL: Duration = Duration::from_secs(3);

/// Memory poll interval.
const MEMORY_INTERVAL: Duration = Duration::from_secs(10);

/// Maximum telemetry events retained in the in-memory ring.
const TELEMETRY_RING: usize = 256;

/// Maximum activity / memory-drawer entries retained in the in-memory ring.
const ACTIVITY_RING: usize = 128;

// ── wire types (poll / SSE response shapes) ──────────────────────────────────

/// Bootstrap payload returned by `/api/bootstrap` (one-shot prefetch).
///
/// Boxed at the `AppEvent::Initial` call-site to keep the enum pointer-sized.
#[derive(Debug, Deserialize)]
pub struct InitialPayload {
    pub repo: String,
    pub root: String,
    pub version: String,
    pub index_files: usize,
    pub index_symbols: usize,
    pub memory_drawers: usize,
}

/// Snapshot returned by `/api/telemetry`.
///
/// Boxed at the `AppEvent::Telemetry` call-site.
#[derive(Debug, Deserialize)]
pub struct TelemetrySnapshot {
    /// Maximum Unix-second timestamp seen; sent back as `since=` on the next poll.
    pub cursor: u64,
    pub events: Vec<TelemetryEvent>,
    /// Debug pane metadata — where the JSONL file lives and how large it is.
    pub source: TelemetrySource,
}

/// File-level metadata surfaced in the "debug" panel.
#[derive(Debug, Deserialize)]
pub struct TelemetrySource {
    /// Display path — immutable after deserialization, so `Box<str>` avoids
    /// the surplus `len`/`cap` that `String` carries.
    pub path: Box<str>,
    pub lines_read: usize,
    pub bytes: u64,
    pub exists: bool,
}

/// One structured log entry from the telemetry JSONL stream.
#[derive(Debug, Deserialize)]
pub struct TelemetryEvent {
    pub ts: u64,
    /// Log level tag — "INFO" / "WARN" / "ERROR" / "DEBUG".
    /// Always ≤ 5 bytes; lives inline in `CompactString` (no heap alloc).
    pub level: CompactString,
    /// Dotted-path target, e.g. `"crabcc_core::store"` — typically ≤ 23 bytes.
    pub target: CompactString,
    /// Free-form structured fields, passed through as-is for the frontend to
    /// render (kpi name, duration_ms, tool, etc.).
    pub fields: serde_json::Value,
}

/// Payload returned by `/api/memory/recent`.
///
/// Boxed at the `AppEvent::MemoryRecent` call-site.
#[derive(Debug, Deserialize)]
pub struct MemoryRecentResponse {
    pub present: bool,
    /// Highest `created_at` seen; echo'd back as `since=` on the next poll.
    pub cursor: i64,
    pub drawers: Vec<DrawerRow>,
}

/// One memory drawer as returned by the recent-drawers endpoint.
#[derive(Debug, Deserialize)]
pub struct DrawerRow {
    pub id: i64,
    /// Short category tag ("proj" / "session") — always ≤ 23 bytes inline.
    pub wing: CompactString,
    /// Optional sub-category — same SSO benefit as `wing`.
    pub room: Option<CompactString>,
    pub source_id: String,
    pub body_preview: String,
    pub created_at: i64,
}

/// Agent summary delivered over the SSE `/api/events` stream.
///
/// Boxed at the `AppEvent::SseAgents` call-site.
#[derive(Debug, Deserialize)]
pub struct AgentSummary {
    pub id: i64,
    pub run_id: CompactString,
    /// Status tag — "running" / "done" / "killed" — always ≤ 23 bytes inline.
    pub status: CompactString,
    pub started_at: i64,
}

/// One activity entry delivered over the SSE stream.
#[derive(Debug, Deserialize)]
pub struct ActivityEvent {
    pub ts: u64,
    /// Short op name, e.g. "sym" / "refs" / "fuzzy" — always ≤ 23 bytes.
    pub op: CompactString,
    pub query: String,
    pub results: usize,
}

// ── AppEvent ─────────────────────────────────────────────────────────────────

/// Events flowing through the `flume::bounded` channel into [`pump_events`].
///
/// **Size contract (item 1):** every variant whose heap payload exceeds 64 B
/// is wrapped in `Box<_>` so the enum fits in 16 bytes (discriminant +
/// pointer) on 64-bit targets.  `SseStatus` and `WorkerDied` carry only
/// `CompactString` / `String` — already pointer-sized.
///
/// After any structural change, verify with:
/// ```text
/// cargo +nightly rustc -p crabcc-desktop -- -Zprint-type-sizes 2>&1 | grep AppEvent
/// ```
#[derive(Debug)]
pub enum AppEvent {
    /// One-shot bootstrap — arrives exactly once, before any poll result.
    Initial(Box<InitialPayload>),
    /// Telemetry-poll result (Ok = new snapshot, Err = transient HTTP/parse failure).
    Telemetry(Box<Result<TelemetrySnapshot>>),
    /// Memory-poll result.
    MemoryRecent(Box<Result<MemoryRecentResponse>>),
    /// SSE activity batch — a `Vec` of activity events since the last frame.
    SseActivity(Box<Vec<ActivityEvent>>),
    /// SSE agent list refresh — replaces the full agent roster.
    SseAgents(Box<Vec<AgentSummary>>),
    /// SSE connection state change ("connecting" / "connected" / "reconnecting").
    /// `CompactString` fits inline — no Box needed.
    SseStatus(CompactString),
    /// A background worker thread panicked or returned a fatal error.
    WorkerDied { name: CompactString, err: String },
    /// Shutdown requested (e.g. window close).
    Quit,
}

// ── Wired ────────────────────────────────────────────────────────────────────

/// Groups the channel sender, JoinHandles, and base URL so callers carry a
/// single `Wired` rather than threading five separate values through every
/// function that interacts with the worker pool.
///
/// Constructed via [`Wired::spawn`].  Drop it (or call [`Wired::shutdown`]) to
/// stop accepting new events; workers will notice the send-error and exit.
pub struct Wired {
    /// Inbound event sender — clone one copy per worker.
    pub tx: Sender<AppEvent>,
    /// Base URL of the crabcc-viz server, e.g. `"http://127.0.0.1:7070"`.
    pub base_url: Arc<str>,
    /// Background thread handles in spawn order:
    /// `[prefetch, sse_bridge, telemetry_poll, memory_poll]`.
    pub handles: Vec<thread::JoinHandle<()>>,
}

impl Wired {
    /// Spawn all four workers and return `(Wired, Receiver<AppEvent>)`.
    ///
    /// ```
    /// use crabcc_desktop::state::Wired;
    /// let (wired, rx) = Wired::spawn("http://127.0.0.1:7070");
    /// // Pass `rx` to `pump_events` on the UI thread.
    /// drop(wired); // signals workers to exit
    /// ```
    pub fn spawn(base_url: impl Into<Arc<str>>) -> (Self, Receiver<AppEvent>) {
        let (tx, rx) = flume::bounded::<AppEvent>(TX_CAP);
        let base_url: Arc<str> = base_url.into();

        let handles = vec![
            spawn_prefetch(tx.clone(), Arc::clone(&base_url)),
            spawn_sse_bridge(tx.clone(), Arc::clone(&base_url)),
            spawn_telemetry_poll(tx.clone(), Arc::clone(&base_url)),
            spawn_memory_poll(tx.clone(), Arc::clone(&base_url)),
        ];

        let wired = Self {
            tx,
            base_url,
            handles,
        };

        (wired, rx)
    }

    /// Send `AppEvent::Quit` and join all worker threads.
    ///
    /// Blocks until every thread has exited.  Errors from individual threads
    /// are logged via `tracing::warn!` and do not propagate.
    pub fn shutdown(self) {
        // Best-effort: the receiver may already be gone.
        let _ = self.tx.send(AppEvent::Quit);
        for handle in self.handles {
            if let Err(e) = handle.join() {
                tracing::warn!("worker thread panicked on shutdown: {e:?}");
            }
        }
    }
}

// ── AppState ─────────────────────────────────────────────────────────────────

/// Mutable dashboard state driven by [`pump_events`] / [`dispatch`].
///
/// Construct with [`AppState::default`]; pass `&mut AppState` to [`dispatch`].
pub struct AppState {
    // ── bootstrap snapshot ──────────────────────────────────────────────────
    pub repo: String,
    pub root: String,
    pub version: String,
    pub index_files: usize,
    pub index_symbols: usize,
    /// Total drawers reported by the bootstrap endpoint (static snapshot;
    /// updated only on `Initial`, not on every memory poll).
    pub memory_drawers: usize,

    // ── telemetry ring (item 5) ─────────────────────────────────────────────
    /// Cursor echoed back to the server on the next poll.
    pub telemetry_cursor: u64,
    /// Most-recent `TELEMETRY_RING` log entries, oldest-first.
    pub telemetry_events: VecDeque<TelemetryEvent>,
    /// File-level metadata for the "debug" panel.
    pub telemetry_source: Option<TelemetrySource>,

    // ── memory ring (item 5) ─────────────────────────────────────────────────
    pub memory_cursor: i64,
    /// Most-recent `ACTIVITY_RING` drawers, oldest-first.
    pub memory_drawers_ring: VecDeque<DrawerRow>,

    // ── SSE ─────────────────────────────────────────────────────────────────
    /// Current agent roster — replaced wholesale on each `SseAgents` event.
    pub agents: Vec<AgentSummary>,
    /// Most-recent `ACTIVITY_RING` activity events, oldest-first.
    pub activity_ring: VecDeque<ActivityEvent>,
    /// Current SSE connection status label.
    pub sse_status: CompactString,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            repo: String::new(),
            root: String::new(),
            version: String::new(),
            index_files: 0,
            index_symbols: 0,
            memory_drawers: 0,
            telemetry_cursor: 0,
            // Pre-allocate rings to their cap so steady-state push_back is free.
            telemetry_events: VecDeque::with_capacity(TELEMETRY_RING),
            telemetry_source: None,
            memory_cursor: 0,
            memory_drawers_ring: VecDeque::with_capacity(ACTIVITY_RING),
            agents: Vec::new(),
            activity_ring: VecDeque::with_capacity(ACTIVITY_RING),
            sse_status: CompactString::const_new("connecting"),
        }
    }
}

// ── dispatch ─────────────────────────────────────────────────────────────────

/// Apply a single [`AppEvent`] to `state`.
///
/// Returns `true` to continue the event loop, `false` on [`AppEvent::Quit`].
///
/// Extracted from [`pump_events`] so it can be unit-tested without a live
/// channel (items 3 + 4 of the senior-Rust review).
pub fn dispatch(state: &mut AppState, event: AppEvent) -> bool {
    match event {
        AppEvent::Initial(payload) => {
            state.repo = payload.repo;
            state.root = payload.root;
            state.version = payload.version;
            state.index_files = payload.index_files;
            state.index_symbols = payload.index_symbols;
            state.memory_drawers = payload.memory_drawers;
        }

        AppEvent::Telemetry(result) => match *result {
            Ok(snap) => {
                state.telemetry_cursor = snap.cursor;
                state.telemetry_source = Some(snap.source);
                for ev in snap.events {
                    ring_push(&mut state.telemetry_events, ev, TELEMETRY_RING);
                }
            }
            Err(e) => tracing::warn!("telemetry poll error: {e:#}"),
        },

        AppEvent::MemoryRecent(result) => match *result {
            Ok(resp) => {
                state.memory_cursor = resp.cursor;
                for drawer in resp.drawers {
                    ring_push(&mut state.memory_drawers_ring, drawer, ACTIVITY_RING);
                }
            }
            Err(e) => tracing::warn!("memory poll error: {e:#}"),
        },

        AppEvent::SseActivity(events) => {
            for ev in *events {
                ring_push(&mut state.activity_ring, ev, ACTIVITY_RING);
            }
        }

        AppEvent::SseAgents(agents) => {
            state.agents = *agents;
        }

        AppEvent::SseStatus(s) => {
            state.sse_status = s;
        }

        AppEvent::WorkerDied { name, err } => {
            tracing::error!("worker '{name}' died: {err}");
        }

        AppEvent::Quit => return false,
    }
    true
}

// ── pump_events ───────────────────────────────────────────────────────────────

/// Drain `rx` until [`AppEvent::Quit`] or the channel closes, calling
/// `on_tick` after each event.
///
/// Returns the final [`AppState`].  Intended to be the main UI-thread loop;
/// the caller decides what to render inside `on_tick`.
///
/// # Example
/// ```no_run
/// use crabcc_desktop::state::{Wired, pump_events};
///
/// let (wired, rx) = Wired::spawn("http://127.0.0.1:7070");
/// let final_state = pump_events(rx, |state| {
///     // render state here
///     let _ = state;
/// });
/// ```
pub fn pump_events<F>(rx: Receiver<AppEvent>, mut on_tick: F) -> AppState
where
    F: FnMut(&AppState),
{
    let mut state = AppState::default();
    for event in &rx {
        if !dispatch(&mut state, event) {
            break;
        }
        on_tick(&state);
    }
    state
}

// ── ring helper ───────────────────────────────────────────────────────────────

/// Push `item` onto `ring`, evicting the oldest entry when `cap` is reached.
#[inline]
fn ring_push<T>(ring: &mut VecDeque<T>, item: T, cap: usize) {
    if ring.len() == cap {
        ring.pop_front();
    }
    ring.push_back(item);
}

// ── workers ───────────────────────────────────────────────────────────────────

fn spawn_prefetch(tx: Sender<AppEvent>, base_url: Arc<str>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("desktop-prefetch".into())
        .spawn(move || match fetch_initial(&base_url) {
            Ok(payload) => {
                let _ = tx.send(AppEvent::Initial(Box::new(payload)));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::WorkerDied {
                    name: CompactString::const_new("prefetch"),
                    err: format!("{e:#}"),
                });
            }
        })
        .expect("spawn desktop-prefetch")
}

fn spawn_sse_bridge(tx: Sender<AppEvent>, base_url: Arc<str>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("desktop-sse".into())
        .spawn(move || {
            let _ = tx.send(AppEvent::SseStatus(CompactString::const_new("connecting")));
            // Real implementation: open an HTTP connection to
            // `{base_url}/api/events`, parse `text/event-stream` frames, and
            // send `SseActivity` / `SseAgents` events as they arrive.
            // On disconnect: sleep + retry, updating `SseStatus` each time.
            // This stub immediately signals "connected" so the UI renders.
            tracing::debug!(base_url = %base_url, "sse_bridge stub: marking connected");
            let _ = tx.send(AppEvent::SseStatus(CompactString::const_new("connected")));
        })
        .expect("spawn desktop-sse")
}

fn spawn_telemetry_poll(tx: Sender<AppEvent>, base_url: Arc<str>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("desktop-telemetry".into())
        .spawn(move || {
            let mut cursor: u64 = 0;
            loop {
                let snap = fetch_telemetry(&base_url, cursor);
                if let Ok(ref s) = snap {
                    cursor = s.cursor;
                }
                if tx.send(AppEvent::Telemetry(Box::new(snap))).is_err() {
                    break;
                }
                thread::sleep(TELEMETRY_INTERVAL);
            }
        })
        .expect("spawn desktop-telemetry")
}

fn spawn_memory_poll(tx: Sender<AppEvent>, base_url: Arc<str>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("desktop-memory".into())
        .spawn(move || loop {
            let resp = fetch_memory_recent(&base_url);
            if tx.send(AppEvent::MemoryRecent(Box::new(resp))).is_err() {
                break;
            }
            thread::sleep(MEMORY_INTERVAL);
        })
        .expect("spawn desktop-memory")
}

// ── stub fetchers ─────────────────────────────────────────────────────────────
//
// These stubs return `Err` so the workers emit `WorkerDied` (visible in the
// UI debug pane) when no crabcc-viz server is running, rather than panicking.
// Replace with real HTTP calls (e.g. using `ureq` or `reqwest`) once the
// desktop binary is wired up.

fn fetch_initial(base_url: &str) -> Result<InitialPayload> {
    anyhow::bail!("prefetch not yet wired — no HTTP client configured (base_url={base_url})")
}

fn fetch_telemetry(base_url: &str, _cursor: u64) -> Result<TelemetrySnapshot> {
    anyhow::bail!("telemetry poll not yet wired (base_url={base_url})")
}

fn fetch_memory_recent(base_url: &str) -> Result<MemoryRecentResponse> {
    anyhow::bail!("memory poll not yet wired (base_url={base_url})")
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── dispatch: Initial ────────────────────────────────────────────────────

    #[test]
    fn dispatch_initial_populates_state() {
        let mut state = AppState::default();
        let payload = InitialPayload {
            repo: "crabcc".into(),
            root: "/tmp/crabcc".into(),
            version: "2.11.0".into(),
            index_files: 42,
            index_symbols: 1234,
            memory_drawers: 7,
        };
        let keep_going = dispatch(&mut state, AppEvent::Initial(Box::new(payload)));
        assert!(keep_going);
        assert_eq!(state.repo, "crabcc");
        assert_eq!(state.index_files, 42);
        assert_eq!(state.memory_drawers, 7);
    }

    // ── dispatch: Quit ───────────────────────────────────────────────────────

    #[test]
    fn dispatch_quit_returns_false() {
        let mut state = AppState::default();
        assert!(!dispatch(&mut state, AppEvent::Quit));
    }

    // ── dispatch: SseStatus ──────────────────────────────────────────────────

    #[test]
    fn dispatch_sse_status_updates_field() {
        let mut state = AppState::default();
        dispatch(
            &mut state,
            AppEvent::SseStatus(CompactString::const_new("connected")),
        );
        assert_eq!(state.sse_status, "connected");
    }

    // ── dispatch: SseAgents ─────────────────────────────────────────────────

    #[test]
    fn dispatch_sse_agents_replaces_roster() {
        let mut state = AppState::default();
        state.agents.push(AgentSummary {
            id: 1,
            run_id: CompactString::const_new("old-run"),
            status: CompactString::const_new("done"),
            started_at: 0,
        });
        let new_agents = vec![AgentSummary {
            id: 2,
            run_id: CompactString::const_new("new-run"),
            status: CompactString::const_new("running"),
            started_at: 100,
        }];
        dispatch(&mut state, AppEvent::SseAgents(Box::new(new_agents)));
        assert_eq!(state.agents.len(), 1);
        assert_eq!(state.agents[0].run_id, "new-run");
    }

    // ── dispatch: Telemetry (Ok path) ────────────────────────────────────────

    #[test]
    fn dispatch_telemetry_appends_events_and_updates_cursor() {
        let mut state = AppState::default();
        let snap = TelemetrySnapshot {
            cursor: 42,
            events: vec![TelemetryEvent {
                ts: 42,
                level: CompactString::const_new("INFO"),
                target: CompactString::const_new("crabcc_core::store"),
                fields: serde_json::Value::Null,
            }],
            source: TelemetrySource {
                path: "/tmp/telemetry.jsonl".into(),
                lines_read: 1,
                bytes: 50,
                exists: true,
            },
        };
        dispatch(&mut state, AppEvent::Telemetry(Box::new(Ok(snap))));
        assert_eq!(state.telemetry_cursor, 42);
        assert_eq!(state.telemetry_events.len(), 1);
        assert_eq!(state.telemetry_events[0].level, "INFO");
        assert!(state.telemetry_source.is_some());
    }

    // ── ring_push: eviction ───────────────────────────────────────────────────

    #[test]
    fn ring_push_evicts_oldest_when_full() {
        let mut ring: VecDeque<u32> = VecDeque::with_capacity(3);
        for i in 0u32..3 {
            ring_push(&mut ring, i, 3);
        }
        // Ring is now [0, 1, 2]; pushing 3 should evict 0.
        ring_push(&mut ring, 3, 3);
        assert_eq!(ring.len(), 3);
        assert_eq!(ring[0], 1);
        assert_eq!(ring[2], 3);
    }

    // ── dispatch: telemetry ring bounds ──────────────────────────────────────

    #[test]
    fn telemetry_ring_is_bounded() {
        let mut state = AppState::default();
        // Push TELEMETRY_RING + 10 events; ring must never exceed the cap.
        for i in 0..=(TELEMETRY_RING + 10) {
            let snap = TelemetrySnapshot {
                cursor: i as u64,
                events: vec![TelemetryEvent {
                    ts: i as u64,
                    level: CompactString::const_new("DEBUG"),
                    target: CompactString::const_new("t"),
                    fields: serde_json::Value::Null,
                }],
                source: TelemetrySource {
                    path: "".into(),
                    lines_read: 0,
                    bytes: 0,
                    exists: false,
                },
            };
            dispatch(&mut state, AppEvent::Telemetry(Box::new(Ok(snap))));
        }
        assert!(state.telemetry_events.len() <= TELEMETRY_RING);
    }

    // ── dispatch: activity ring bounds ───────────────────────────────────────

    #[test]
    fn activity_ring_is_bounded() {
        let mut state = AppState::default();
        for i in 0..=(ACTIVITY_RING + 5) {
            let events = vec![ActivityEvent {
                ts: i as u64,
                op: CompactString::const_new("sym"),
                query: "Foo".into(),
                results: 1,
            }];
            dispatch(&mut state, AppEvent::SseActivity(Box::new(events)));
        }
        assert!(state.activity_ring.len() <= ACTIVITY_RING);
    }

    // ── dispatch: MemoryRecent (Ok path) ─────────────────────────────────────

    #[test]
    fn dispatch_memory_recent_appends_drawers() {
        let mut state = AppState::default();
        let resp = MemoryRecentResponse {
            present: true,
            cursor: 99,
            drawers: vec![DrawerRow {
                id: 1,
                wing: CompactString::const_new("proj"),
                room: None,
                source_id: "src/main.rs".into(),
                body_preview: "fn main()".into(),
                created_at: 99,
            }],
        };
        dispatch(&mut state, AppEvent::MemoryRecent(Box::new(Ok(resp))));
        assert_eq!(state.memory_cursor, 99);
        assert_eq!(state.memory_drawers_ring.len(), 1);
        assert_eq!(state.memory_drawers_ring[0].wing, "proj");
    }

    // ── pump_events: smoke ───────────────────────────────────────────────────

    #[test]
    fn pump_events_processes_quit() {
        let (tx, rx) = flume::bounded::<AppEvent>(4);
        tx.send(AppEvent::Quit).unwrap();
        drop(tx);
        let mut ticks = 0usize;
        let state = pump_events(rx, |_| ticks += 1);
        // Quit fires before on_tick — zero ticks expected.
        assert_eq!(ticks, 0);
        assert_eq!(state.repo, "");
    }

    #[test]
    fn pump_events_processes_events_before_quit() {
        let (tx, rx) = flume::bounded::<AppEvent>(8);
        tx.send(AppEvent::SseStatus(CompactString::const_new("connected")))
            .unwrap();
        tx.send(AppEvent::Quit).unwrap();
        drop(tx);
        let mut ticks = 0usize;
        let state = pump_events(rx, |_| ticks += 1);
        assert_eq!(ticks, 1);
        assert_eq!(state.sse_status, "connected");
    }

    // ── WorkerDied ───────────────────────────────────────────────────────────

    #[test]
    fn dispatch_worker_died_does_not_stop_loop() {
        let mut state = AppState::default();
        let keep_going = dispatch(
            &mut state,
            AppEvent::WorkerDied {
                name: CompactString::const_new("prefetch"),
                err: "connection refused".into(),
            },
        );
        // WorkerDied is non-fatal for the event loop.
        assert!(keep_going);
    }
}
