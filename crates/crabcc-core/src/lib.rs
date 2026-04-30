//! `crabcc-core` — the indexing, storage, and query primitives behind the
//! `crabcc` CLI / MCP server.
//!
//! Per-repo state lives at `<repo>/.crabcc/`:
//!
//! - `index.db` — SQLite store: `files`, `symbols`, `edges`, `meta`.
//!   Always built additively; schema upgrades happen in
//!   [`store::Store::open`] (see also the `signature_enc` and edge-shape
//!   migrations).
//! - `tantivy/` — sidecar full-text index for fuzzy + prefix search (see
//!   [`fts`]). Rebuilt from the SQLite store on `crabcc index`; refresh
//!   deliberately doesn't.
//! - `graph.json` — call-graph sidecar built from the populated `edges`
//!   table (see [`graph::CallGraph`]).
//! - `fsst.symbols` — optional FSST codec table for signature-column
//!   compression (see [`compress`], gated behind the `compress` cargo
//!   feature, default ON).
//!
//! ## Modules at a glance
//!
//! | Module        | What it owns |
//! |---------------|--------------|
//! | [`store`]     | Schema bootstrap + connection setup; CRUD on `files`, `symbols`, `edges`, `meta`. |
//! | [`extract`]   | Tree-sitter symbol extractors (TS/TSX/JS, Ruby, Rust, Go, Python). |
//! | [`index`]     | `full_index` and `refresh` — the two indexer entry points. |
//! | [`query`]     | `find_symbol`, `query_callers`, `query_refs`, plus the shaping `Mode` enum. |
//! | [`graph`]     | [`graph::CallGraph`]: build / save / walk / cycles / orphans. |
//! | [`outline`]   | File-level top-symbol list (no bodies). |
//! | [`pattern`]   | Per-language ast-grep patterns + `lang_for` resolver. |
//! | [`fts`]       | Tantivy sidecar — fuzzy (Levenshtein 2) and prefix lookup. |
//! | [`refs`]      | Streaming ref/grep adapter (`grep::searcher` + memchr). |
//! | [`compress`]  | FSST codec — train / encode / decode / round-trip. Feature-gated. |
//! | [`hash`]      | `sha256_hex` — content-addressed file dedup. |
//! | [`track`]     | Token-savings telemetry written to `.crabcc/track.json`. |
//! | [`walker`]    | `walk_repo` — gitignore-aware iterator over the repo tree. |
//! | [`watch`]     | `notify-debouncer-mini`-backed file-watcher hook. |
//! | [`upgrade`]   | Compare local crate version to the latest GitHub release. |
//! | [`types`]     | Shared types: [`Symbol`], [`Edge`], [`Hit`], [`SymbolKind`]. |
//!
//! ## Quick example — index a directory and look up a symbol
//!
//! ```no_run
//! use crabcc_core::{index, query, store::Store};
//! use std::path::Path;
//!
//! let root = Path::new(".");
//! let db = root.join(".crabcc/index.db");
//! let store = Store::open(&db).expect("open store");
//! let _stats = index::full_index(root, &store).expect("index");
//! let hits = query::find_symbol(&store, "Foo").expect("query");
//! for s in hits {
//!     println!("{} @ {}:{}", s.name, s.file, s.line_start);
//! }
//! ```
//!
//! ## Cargo features
//!
//! - `compress` — pulls in `fsst-rs` and enables the FSST codec for
//!   `signatures.signature_enc`. Default ON. Disable with
//!   `--no-default-features` to drop the dep.

#[cfg(feature = "compress")]
pub mod compress;
pub mod config;
pub mod extract;
pub mod fts;
pub mod gitdiff;
pub mod graph;
pub mod hash;
pub mod index;
#[cfg(feature = "jobs")]
pub mod jobs;
#[cfg(feature = "markdown")]
pub mod md;
pub mod ollama_stack;
pub mod outline;
pub mod pattern;
pub mod query;
pub mod refs;
pub mod store;
pub mod track;
pub mod types;
pub mod upgrade;
pub mod walker;
pub mod watch;

pub use types::{Edge, Hit, Symbol, SymbolKind};
