//! Library surface of `ucracc-lsp`. The `ucracc-lsp` binary in
//! `src/main.rs` is a thin wrapper; the actual server, handlers, and
//! per-language extractors live here so they can be unit/integration
//! tested without spawning a subprocess.

pub mod cache;
pub mod commands;
pub mod handlers;
pub mod incremental;
pub mod lang;
#[cfg(feature = "markdown")]
pub mod markdown;
#[cfg(feature = "rerank")]
pub mod rerank;
pub mod server;
#[cfg(feature = "yaml")]
pub mod yaml;
