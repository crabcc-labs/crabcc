//! crabcc memory — local-first AI memory layer.
//!
//! M0: trait surface + in-memory + file-backed SQLite backends + Palace facade.
//! M0.5 swaps in `sqlite-vec` for fast ANN; M1 adds `fastembed-rs` for real
//! semantic embeddings. The `Backend` and `Embedder` traits are stable.
//!
//! Per-repo by design: `Palace::open(repo_root)` creates or reuses
//! `<repo_root>/.crabcc/memory.db`.

pub mod backend;
pub mod embed;
pub mod palace;
pub mod types;

pub use backend::{in_memory::InMemoryBackend, sqlite::SqliteBackend, Backend, LexicalQuery};
pub use embed::{Embedder, HashEmbedder};
pub use palace::{find_git_root, Palace, PalaceRegistry, SearchMode};
pub use types::{
    DeleteSel, Drawer, DrawerHit, DrawerId, DrawerInsert, GetResult, HealthStatus, Query,
    QueryResult, Session, Wing,
};
