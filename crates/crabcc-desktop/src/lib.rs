//! `crabcc-desktop` — single-window dashboard state model.
//!
//! Public surface: [`state`] module.
//!
//! See [`state::Wired::spawn`] for the canonical entry point that boots all
//! four background workers and returns the inbound [`state::AppEvent`] receiver.

pub mod state;

pub use state::{
    dispatch, pump_events, ActivityEvent, AgentSummary, AppEvent, AppState, DrawerRow,
    InitialPayload, MemoryRecentResponse, TelemetryEvent, TelemetrySnapshot, TelemetrySource,
    Wired,
};
