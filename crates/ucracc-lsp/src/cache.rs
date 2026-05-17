//! In-process LRU for repeated identical LSP queries. Read-side only —
//! every document write (didOpen / didChange / didSave) flushes the
//! cache, so we never return stale results.
//!
//! Sized small on purpose: the values are Arc-wrapped serde JSON,
//! shareable across handler calls without cloning the underlying Vec.
//! The point isn't bulk storage, it's collapsing the 6–10 µs SQLite hop
//! on the *same* request being issued repeatedly (cursor hover loops,
//! workspace/symbol auto-completion while typing, etc.).

use moka::sync::Cache;
use std::sync::Arc;
use std::time::Duration;

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

pub type Value = Arc<serde_json::Value>;

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
