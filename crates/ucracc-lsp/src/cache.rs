//! In-process LRU for repeated identical LSP queries. Read-side only —
//! every document write (didOpen / didChange / didSave) flushes the
//! cache, so we never return stale results.
//!
//! Values are Arc-wrapped typed payloads (`Vec<Location>`,
//! `Vec<DocumentSymbol>`, …), shareable across handler calls without
//! cloning the underlying Vec. The point isn't bulk storage, it's
//! collapsing the 6–10 µs SQLite hop on the *same* request being issued
//! repeatedly (cursor hover loops, workspace/symbol auto-completion
//! while typing, etc.). Typed values skip the `serde_json::from_value`
//! parse (~500 ns) on every cache hit vs the prior
//! `Arc<serde_json::Value>` design.

use moka::sync::Cache;
use std::sync::Arc;
use std::time::Duration;
use tower_lsp::lsp_types::{DocumentSymbol, Hover, Location, SymbolInformation};

#[derive(Clone, Hash, PartialEq, Eq)]
pub enum Key {
    Definition(String),
    Hover(String),
    DocumentSymbols(String),
    WorkspaceSymbol { query: String, limit: u32 },
    OutgoingCalls(String),
    IncomingCalls(String),
    References(String),
}

/// Typed cache payload — one variant per LSP method's response shape.
/// Cloning a `Value` is cheap (it's an enum of `Arc`s).
#[derive(Clone)]
pub enum Value {
    Definition(Arc<Vec<Location>>),
    Hover(Arc<Option<Hover>>),
    DocumentSymbols(Arc<Vec<DocumentSymbol>>),
    WorkspaceSymbol(Arc<Vec<SymbolInformation>>),
    References(Arc<Vec<Location>>),
}

pub struct LruCache {
    inner: Cache<Key, Value>,
}

impl LruCache {
    pub fn new() -> Self {
        Self {
            inner: Cache::builder()
                .max_capacity(1024)
                // Backstop: even if we miss an invalidation event, results
                // expire after 30 s.
                .time_to_live(Duration::from_secs(30))
                .build(),
        }
    }

    pub fn get(&self, key: &Key) -> Option<Value> {
        self.inner.get(key)
    }

    pub fn put(&self, key: Key, value: Value) {
        self.inner.insert(key, value);
    }

    /// Flush everything. Cheap (just drops the segment maps).
    pub fn invalidate_all(&self) {
        self.inner.invalidate_all();
    }

    #[cfg(test)]
    pub fn entry_count(&self) -> u64 {
        self.inner.run_pending_tasks();
        self.inner.entry_count()
    }
}

impl Default for LruCache {
    fn default() -> Self {
        Self::new()
    }
}
