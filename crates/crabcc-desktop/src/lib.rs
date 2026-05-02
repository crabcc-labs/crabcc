//! `crabcc-desktop` — GPUI-rendered native dashboard.
//!
//! The library half exposes the API client + future AppState so
//! integration tests (and the bin half) consume the same surface. UI
//! rendering lives entirely in `main.rs` for now; routes get split out
//! in phase A.3+.

pub mod api;
