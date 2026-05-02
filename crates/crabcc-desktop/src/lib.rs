//! `crabcc-desktop` — GPUI-rendered native dashboard.
//!
//! The library half exposes the API client + SSE bridge so
//! integration tests (and the bin half) consume the same surface. UI
//! rendering lives entirely in `main.rs` for now; routes get split out
//! in phase A.4+.

pub mod api;
pub mod sse;
