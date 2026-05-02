//! Typed client over `crabcc serve`'s `/api/*` HTTP surface.
//!
//! Mirrors `crates/crabcc-viz/web/src/api.ts` so a UI engineer
//! switching between the React and GPUI surfaces sees the same shape.
//! Streaming / SSE moves to `super::sse` in phase A.3.

pub mod client;
pub mod types;

pub use client::{Client, DEFAULT_BASE_URL};
