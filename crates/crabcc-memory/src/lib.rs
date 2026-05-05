// `simd-cosine` opts into nightly's `portable_simd` (rust-lang/rust#86656)
// for the brute-force cosine helper at backend/mod.rs. Default OFF — stable
// users keep working unchanged; nightly users get the speed-up.
#![cfg_attr(feature = "simd-cosine", feature(portable_simd))]
//! `crabcc-memory` — local-first AI memory layer.
//!
//! Drawers are SHA-content-addressed snippets stored at
//! `<repo>/.crabcc/memory.db` and grouped into `wings` (top-level buckets)
//! and `rooms` (sub-buckets). Storage and retrieval go through the
//! [`Backend`] trait; the [`Palace`] facade is the recommended entry
//! point for callers (CLI, MCP server, future SDKs).
//!
//! ## Layers
//!
//! | Module            | What it owns |
//! |-------------------|--------------|
//! | [`palace`]        | [`Palace`] facade + multi-project [`PalaceRegistry`]. Idempotent open at `<repo>/.crabcc/memory.db`. |
//! | [`backend`]       | The [`Backend`] trait + two impls: [`SqliteBackend`] (durable; FSST + FTS5 on top) and [`InMemoryBackend`] (tests, ephemeral). |
//! | [`embed`]         | The [`Embedder`] trait + [`HashEmbedder`] (deterministic stub used in tests + as the M0/M1a default). |
//! | [`types`]         | Public data types: [`Drawer`], [`DrawerInsert`], [`Wing`], [`Query`], [`QueryResult`], [`Session`], [`HealthStatus`]. |
//!
//! ## Quick example — open a palace, store + search
//!
//! ```no_run
//! use crabcc_memory::Palace;
//! use std::path::Path;
//!
//! let palace = Palace::open(Path::new(".")).expect("open palace");
//! palace.remember("notes", None, "doc:1", "the quick brown fox").unwrap();
//! palace.remember("notes", None, "doc:2", "lazy dogs sleep all day").unwrap();
//!
//! // Default search is hybrid: BM25 + vector via Reciprocal Rank Fusion.
//! let hits = palace.search("fox", 5).expect("search").hits;
//! assert!(hits.iter().any(|h| h.source_id == "doc:1"));
//! ```
//!
//! ## Search modes
//!
//! [`Palace::search`] now defaults to [`SearchMode::Hybrid`]. For
//! ablation, use [`Palace::search_with_mode`] with one of:
//!
//! - [`SearchMode::Hybrid`] — BM25 (FTS5) + vector (cosine KNN) fused via
//!   Reciprocal Rank Fusion (k = 60).
//! - [`SearchMode::Lexical`] — BM25 only (good for keyword queries).
//! - [`SearchMode::Vector`] — cosine KNN only (good for semantic queries
//!   when a real embedder is plugged in).
//!
//! ## Roadmap
//!
//! - **M0** — `Backend` trait + persistent `SqliteBackend` + `Palace`
//!   facade + sessions ✅
//! - **M0.5** — `sqlite-vec` ANN backend behind the `memory-vec`
//!   feature ✅ (issue #17)
//! - **M1a** — FTS5 + RRF hybrid search ✅
//! - **M1b** — `FastEmbedder` (fastembed-rs / MiniLM-L6-v2) behind the
//!   `memory-embed` feature ✅ (issue #18)
//! - **M2**   — miners (`crabcc memory mine project|sessions`)
//! - **Bench** — LongMemEval R@5 ≥ 96.6% gate (issue #2)
//!
//! Per-repo by design: `Palace::open(repo_root)` creates or reuses
//! `<repo_root>/.crabcc/memory.db`. The MCP server uses
//! [`PalaceRegistry`] to multiplex many palaces by canonical git root.
//!
//! ## Cargo features
//!
//! - `compress` — forwards `crabcc-core/compress` so drawer bodies share
//!   the same FSST codec used by the symbol-store. Default ON.
//! - `memory-vec` — link the bundled `sqlite-vec` extension for ANN
//!   queries. Default OFF.
//! - `memory-embed` — pull in `fastembed-rs` and expose `FastEmbedder`
//!   for real semantic 384-dim MiniLM-L6-v2 vectors. Default OFF
//!   (~25 MB ONNX model file lazy-downloaded on first use).

pub mod backend;
pub mod embed;
pub mod mine;
pub mod palace;
pub mod read;
pub mod shell;
#[cfg(test)]
pub(crate) mod test_support;
pub mod types;

pub use backend::{in_memory::InMemoryBackend, sqlite::SqliteBackend, Backend, LexicalQuery};
#[cfg(feature = "memory-embed")]
pub use embed::FastEmbedder;
pub use embed::{CachedEmbedder, Embedder, HashEmbedder, DEFAULT_EMBED_CACHE_CAPACITY};
pub use mine::{
    project::{mine_project, MineProjectOpts, DEFAULT_MAX_FILE_BYTES},
    sessions::{mine_sessions, MineSessionsOpts, DEFAULT_MAX_PAIR_BYTES},
    MineReport,
};
pub use palace::{
    find_git_root, resolve_db_path, Palace, PalaceRegistry, SearchMode,
    DEFAULT_PALACE_CACHE_CAPACITY, GIT_ROOT_CACHE_TTL, PALACE_CACHE_TTI,
};
pub use types::{
    DeleteSel, Drawer, DrawerHit, DrawerId, DrawerInsert, GetResult, HealthStatus, Query,
    QueryResult, Session, Wing,
};
