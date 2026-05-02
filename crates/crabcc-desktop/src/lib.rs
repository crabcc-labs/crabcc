//! `crabcc-desktop` — GPUI-rendered native dashboard.
//!
//! Module map:
//!   - `api`     — typed HTTP client + wire types (A.2)
//!   - `sse`     — long-lived SSE worker, smol-friendly via `flume` (A.3)
//!   - `state`   — `AppState` entity + dual-worker bridge (A.4)
//!   - `routes`  — top-level Render views per dashboard route (A.4+)

pub mod api;
pub mod routes;
pub mod sse;
pub mod state;
