//! `crabcc-desktop` — GPUI-rendered native dashboard.
//!
//! Module map:
//!   - `api`           — typed HTTP client + wire types (A.2)
//!   - `sse`           — long-lived SSE worker, smol-friendly via `flume` (A.3)
//!   - `state`         — `AppState` entity + dual-worker bridge + `Route` (A.4 / A.6)
//!   - `routes`        — body content views per dashboard route (A.4+)
//!   - `graph_layout`  — pure-compute force-directed layout (A.5)
//!   - `shell`         — top-level header + nav + body switcher (A.6)

pub mod api;
pub mod graph_layout;
pub mod routes;
pub mod shell;
pub mod sse;
pub mod state;
