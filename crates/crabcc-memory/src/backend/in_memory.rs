//! In-memory `Backend` — `HashMap` + brute-force cosine.
//!
//! Use cases: unit tests of the trait surface, ephemeral palaces (see
//! `Palace::ephemeral`). Not durable across process exits. Thread-safe via
//! an inner `Mutex`. Dedup keyed on `(source_id, sha256(body))` — re-adding
//! the same drawer returns its existing id rather than creating a duplicate.

use crate::backend::{cosine, Backend, LexicalQuery};
use crate::types::*;
use ahash::HashMap;
use anyhow::{anyhow, Result};
use crabcc_core::hash::sha256_hex;
use std::sync::Mutex;

#[derive(Default)]
struct Inner {
    next_id: i64,
    rows: HashMap<DrawerId, Stored>,
    sha_index: HashMap<(String, String), DrawerId>,
    next_reminder_id: i64,
    reminders: Vec<Reminder>,
}

struct Stored {
    drawer: Drawer,
    embedding: Vec<f32>,
}

/// Pure-Rust in-memory backend. Brute-force cosine. For tests and short-
/// lived ephemeral palaces. No persistence.
pub struct InMemoryBackend {
    inner: Mutex<Inner>,
}

impl InMemoryBackend {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }
}

impl Default for InMemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn now_secs() -> i64 {
    crabcc_core::time::unix_now_secs() as i64
}

fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

impl Backend for InMemoryBackend {
    fn add(&self, drawers: &[DrawerInsert]) -> Result<Vec<DrawerId>> {
        let mut inner = self.inner.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let mut ids = Vec::with_capacity(drawers.len());
        let now = now_secs();
        for d in drawers {
            let sha = sha256_hex(d.body.as_bytes());
            let key = (d.source_id.clone(), sha.clone());
            if let Some(&existing) = inner.sha_index.get(&key) {
                ids.push(existing);
                continue;
            }
            inner.next_id += 1;
            let id = inner.next_id;
            inner.sha_index.insert(key, id);
            inner.rows.insert(
                id,
                Stored {
                    drawer: Drawer {
                        id,
                        wing: d.wing.clone(),
                        room: d.room.clone(),
                        source_id: d.source_id.clone(),
                        body: d.body.clone(),
                        sha256: sha,
                        created_at: now,
                        session_id: d.session_id.clone(),
                    },
                    embedding: d.embedding.clone(),
                },
            );
            ids.push(id);
        }
        Ok(ids)
    }

    fn query(&self, q: &Query) -> Result<QueryResult> {
        let inner = self.inner.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let mut scored: Vec<(f32, &Stored)> = inner
            .rows
            .values()
            .filter(|s| q.wing.as_ref().is_none_or(|w| &s.drawer.wing == w))
            .filter(|s| {
                q.room
                    .as_ref()
                    .is_none_or(|r| s.drawer.room.as_deref() == Some(r.as_str()))
            })
            .map(|s| (cosine(&q.embedding, &s.embedding), s))
            .collect();
        scored.sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(q.limit);
        Ok(QueryResult {
            hits: scored
                .into_iter()
                .map(|(score, s)| DrawerHit {
                    id: s.drawer.id,
                    score,
                    source_id: s.drawer.source_id.clone(),
                    body: s.drawer.body.clone(),
                    wing: s.drawer.wing.clone(),
                    room: s.drawer.room.clone(),
                })
                .collect(),
        })
    }

    fn query_lexical(&self, q: &LexicalQuery) -> Result<QueryResult> {
        // Token-overlap scoring — a tiny stand-in for FTS5/BM25 used only
        // by ephemeral palaces and unit tests. Score = (#query tokens that
        // appear at least once in body) / (#query tokens). Drawers with
        // zero matches are dropped — RRF only sees rows that the lexical
        // path actually retrieved, mirroring the SQL `MATCH` semantics.
        let q_tokens = tokenize(&q.text);
        if q_tokens.is_empty() {
            return Ok(QueryResult::default());
        }
        let inner = self.inner.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let mut scored: Vec<(f32, &Stored)> = inner
            .rows
            .values()
            .filter(|s| q.wing.as_ref().is_none_or(|w| &s.drawer.wing == w))
            .filter(|s| {
                q.room
                    .as_ref()
                    .is_none_or(|r| s.drawer.room.as_deref() == Some(r.as_str()))
            })
            .filter_map(|s| {
                let body_lower = s.drawer.body.to_lowercase();
                let body_tokens: std::collections::HashSet<String> =
                    tokenize(&body_lower).into_iter().collect();
                let hit = q_tokens.iter().filter(|t| body_tokens.contains(*t)).count();
                if hit == 0 {
                    None
                } else {
                    Some((hit as f32 / q_tokens.len() as f32, s))
                }
            })
            .collect();
        scored.sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(q.limit);
        Ok(QueryResult {
            hits: scored
                .into_iter()
                .map(|(score, s)| DrawerHit {
                    id: s.drawer.id,
                    score,
                    source_id: s.drawer.source_id.clone(),
                    body: s.drawer.body.clone(),
                    wing: s.drawer.wing.clone(),
                    room: s.drawer.room.clone(),
                })
                .collect(),
        })
    }

    fn get(&self, ids: &[DrawerId]) -> Result<GetResult> {
        let inner = self.inner.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        Ok(GetResult {
            drawers: ids
                .iter()
                .filter_map(|id| inner.rows.get(id).map(|s| s.drawer.clone()))
                .collect(),
        })
    }

    fn delete(&self, sel: &DeleteSel) -> Result<usize> {
        let mut inner = self.inner.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let before = inner.rows.len();
        match sel {
            DeleteSel::All => {
                inner.rows.clear();
                inner.sha_index.clear();
            }
            DeleteSel::ById(ids) => {
                for id in ids {
                    if let Some(s) = inner.rows.remove(id) {
                        inner
                            .sha_index
                            .remove(&(s.drawer.source_id, s.drawer.sha256));
                    }
                }
            }
            DeleteSel::BySource(src) => {
                let to_drop: Vec<DrawerId> = inner
                    .rows
                    .values()
                    .filter(|s| &s.drawer.source_id == src)
                    .map(|s| s.drawer.id)
                    .collect();
                for id in to_drop {
                    if let Some(s) = inner.rows.remove(&id) {
                        inner
                            .sha_index
                            .remove(&(s.drawer.source_id, s.drawer.sha256));
                    }
                }
            }
            DeleteSel::BeforeInWing { wing, before } => {
                let to_drop: Vec<DrawerId> = inner
                    .rows
                    .values()
                    .filter(|s| &s.drawer.wing == wing && s.drawer.created_at < *before)
                    .map(|s| s.drawer.id)
                    .collect();
                for id in to_drop {
                    if let Some(s) = inner.rows.remove(&id) {
                        inner
                            .sha_index
                            .remove(&(s.drawer.source_id, s.drawer.sha256));
                    }
                }
            }
        }
        Ok(before - inner.rows.len())
    }

    fn count(&self) -> Result<usize> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| anyhow!("poisoned mutex"))?
            .rows
            .len())
    }

    fn health(&self) -> HealthStatus {
        HealthStatus::Ok
    }

    fn list_drawers(&self, wing: Option<&str>, limit: usize) -> Result<Vec<Drawer>> {
        let inner = self.inner.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let mut rows: Vec<Drawer> = inner
            .rows
            .values()
            .filter(|s| wing.is_none_or(|w| s.drawer.wing == w))
            .map(|s| s.drawer.clone())
            .collect();
        rows.sort_unstable_by_key(|d| d.id);
        if limit > 0 {
            rows.truncate(limit);
        }
        Ok(rows)
    }

    fn remind_set(&self, due_at: i64, message: &str) -> Result<i64> {
        let mut inner = self.inner.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        inner.next_reminder_id += 1;
        let id = inner.next_reminder_id;
        inner.reminders.push(Reminder {
            id,
            due_at,
            message: message.to_string(),
            created_at: now_secs(),
            delivered: false,
        });
        Ok(id)
    }

    fn remind_poll(&self) -> Result<Vec<Reminder>> {
        let mut inner = self.inner.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let now = now_secs();
        let mut due = Vec::new();
        for r in &mut inner.reminders {
            if r.due_at <= now && !r.delivered {
                r.delivered = true;
                due.push(r.clone());
            }
        }
        due.sort_unstable_by_key(|r| r.due_at);
        Ok(due)
    }

    fn remind_list(&self, include_delivered: bool) -> Result<Vec<Reminder>> {
        let inner = self.inner.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let mut rows: Vec<Reminder> = inner
            .reminders
            .iter()
            .filter(|r| include_delivered || !r.delivered)
            .cloned()
            .collect();
        rows.sort_unstable_by_key(|r| r.due_at);
        Ok(rows)
    }

    fn remind_delete(&self, id: i64) -> Result<bool> {
        let mut inner = self.inner.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let before = inner.reminders.len();
        inner.reminders.retain(|r| r.id != id);
        Ok(inner.reminders.len() < before)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::{Embedder, HashEmbedder};

    fn ins(emb: &HashEmbedder, source: &str, body: &str, wing: &str) -> DrawerInsert {
        DrawerInsert {
            wing: wing.into(),
            room: None,
            source_id: source.into(),
            body: body.into(),
            embedding: emb.embed_one(body).unwrap(),
            session_id: None,
        }
    }

    #[test]
    fn add_and_count() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        let ids = b
            .add(&[
                ins(&e, "a", "hello", "default"),
                ins(&e, "b", "world", "default"),
            ])
            .unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(b.count().unwrap(), 2);
    }

    #[test]
    fn dedup_by_source_and_sha() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        let id1 = b.add(&[ins(&e, "a", "same", "d")]).unwrap()[0];
        let id2 = b.add(&[ins(&e, "a", "same", "d")]).unwrap()[0];
        assert_eq!(id1, id2);
        assert_eq!(b.count().unwrap(), 1);
    }

    #[test]
    fn self_query_ranks_first() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        b.add(&[
            ins(&e, "1", "fox jumps", "d"),
            ins(&e, "2", "cat sleeps", "d"),
            ins(&e, "3", "dog runs", "d"),
        ])
        .unwrap();
        let q = Query {
            embedding: e.embed_one("fox jumps").unwrap(),
            limit: 3,
            wing: None,
            room: None,
        };
        let r = b.query(&q).unwrap();
        assert_eq!(r.hits[0].body, "fox jumps");
        assert!(r.hits[0].score > 0.99);
    }

    #[test]
    fn wing_filter() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        b.add(&[ins(&e, "1", "alpha", "wa"), ins(&e, "2", "beta", "wb")])
            .unwrap();
        let q = Query {
            embedding: e.embed_one("alpha").unwrap(),
            limit: 10,
            wing: Some("wb".into()),
            room: None,
        };
        let r = b.query(&q).unwrap();
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].wing, "wb");
    }

    #[test]
    fn delete_by_source() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        b.add(&[
            ins(&e, "a", "1", "d"),
            ins(&e, "a", "2", "d"),
            ins(&e, "b", "3", "d"),
        ])
        .unwrap();
        assert_eq!(b.delete(&DeleteSel::BySource("a".into())).unwrap(), 2);
        assert_eq!(b.count().unwrap(), 1);
    }

    #[test]
    fn session_id_round_trips() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        let mut d = ins(&e, "a", "x", "d");
        d.session_id = Some("term:abc".into());
        let id = b.add(&[d]).unwrap()[0];
        let g = b.get(&[id]).unwrap();
        assert_eq!(g.drawers[0].session_id.as_deref(), Some("term:abc"));
    }

    #[test]
    fn add_empty_returns_empty_vec() {
        let b = InMemoryBackend::new();
        assert!(b.add(&[]).unwrap().is_empty());
        assert_eq!(b.count().unwrap(), 0);
    }

    #[test]
    fn query_with_no_drawers_returns_empty() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        let q = Query {
            embedding: e.embed_one("anything").unwrap(),
            limit: 5,
            wing: None,
            room: None,
        };
        assert!(b.query(&q).unwrap().hits.is_empty());
    }

    #[test]
    fn list_drawers_includes_session_id() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        let mut d = ins(&e, "doc:1", "body", "default");
        d.session_id = Some("s1".into());
        b.add(&[d]).unwrap();
        let listed = b.list_drawers(None, 10).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn list_drawers_wing_filter_preserves_session() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        let mut d1 = ins(&e, "1", "alpha", "wing-a");
        d1.session_id = Some("shared".into());
        let mut d2 = ins(&e, "2", "beta", "wing-b");
        d2.session_id = Some("shared".into());
        b.add(&[d1, d2]).unwrap();
        let only_a = b.list_drawers(Some("wing-a"), 10).unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].session_id.as_deref(), Some("shared"));
    }

    #[test]
    fn delete_all_clears_dedup_index() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        let id1 = b.add(&[ins(&e, "x", "body", "d")]).unwrap()[0];
        b.delete(&DeleteSel::All).unwrap();
        // After All, re-adding the "same" drawer must mint a NEW id — the
        // sha index was wiped along with the rows.
        let id2 = b.add(&[ins(&e, "x", "body", "d")]).unwrap()[0];
        assert_ne!(id1, id2);
        assert_eq!(b.count().unwrap(), 1);
    }

    // ---------- M1: lexical search ----------

    #[test]
    fn lexical_returns_only_matches() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        b.add(&[
            ins(&e, "1", "fox jumps over fence", "d"),
            ins(&e, "2", "cat sleeps in sun", "d"),
            ins(&e, "3", "fox in henhouse", "d"),
        ])
        .unwrap();
        let r = b
            .query_lexical(&LexicalQuery {
                text: "fox".into(),
                limit: 10,
                wing: None,
                room: None,
            })
            .unwrap();
        let bodies: std::collections::HashSet<&str> =
            r.hits.iter().map(|h| h.body.as_str()).collect();
        assert_eq!(r.hits.len(), 2);
        assert!(bodies.contains("fox jumps over fence"));
        assert!(bodies.contains("fox in henhouse"));
    }

    #[test]
    fn lexical_empty_query_returns_empty() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        b.add(&[ins(&e, "1", "anything", "d")]).unwrap();
        let r = b
            .query_lexical(&LexicalQuery {
                text: "   ".into(),
                limit: 10,
                wing: None,
                room: None,
            })
            .unwrap();
        assert!(r.hits.is_empty());
    }

    #[test]
    fn lexical_score_proportional_to_token_overlap() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        // d1 hits 2/2 tokens, d2 hits 1/2.
        b.add(&[
            ins(&e, "1", "needle haystack", "d"),
            ins(&e, "2", "haystack only", "d"),
        ])
        .unwrap();
        let r = b
            .query_lexical(&LexicalQuery {
                text: "needle haystack".into(),
                limit: 5,
                wing: None,
                room: None,
            })
            .unwrap();
        assert_eq!(r.hits.len(), 2);
        assert_eq!(r.hits[0].source_id, "1");
        assert!(r.hits[0].score > r.hits[1].score);
    }

    #[test]
    fn lexical_wing_filter() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        b.add(&[ins(&e, "1", "alpha", "wa"), ins(&e, "2", "alpha", "wb")])
            .unwrap();
        let r = b
            .query_lexical(&LexicalQuery {
                text: "alpha".into(),
                limit: 10,
                wing: Some("wb".into()),
                room: None,
            })
            .unwrap();
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].wing, "wb");
    }

    #[test]
    fn delete_before_in_wing_removes_old_entries() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        // Add two drawers to the same wing.
        b.add(&[
            ins(&e, "old", "old content", "proj"),
            ins(&e, "new", "new content", "proj"),
        ])
        .unwrap();
        // Get the created_at timestamps for the drawers.
        let drawers = b.list_drawers(Some("proj"), 10).unwrap();
        assert_eq!(drawers.len(), 2);
        // Use a "before" timestamp that is far in the future, so both entries
        // qualify as old.
        let far_future = drawers.iter().map(|d| d.created_at).max().unwrap() + 9999;
        let removed = b
            .delete(&DeleteSel::BeforeInWing {
                wing: "proj".into(),
                before: far_future,
            })
            .unwrap();
        assert_eq!(removed, 2);
        assert_eq!(b.count().unwrap(), 0);
    }

    #[test]
    fn delete_before_in_wing_respects_wing_filter() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        b.add(&[
            ins(&e, "a", "content a", "wing-a"),
            ins(&e, "b", "content b", "wing-b"),
        ])
        .unwrap();
        let far_future = 9_999_999_999_i64;
        // Delete before far_future in wing-a only.
        let removed = b
            .delete(&DeleteSel::BeforeInWing {
                wing: "wing-a".into(),
                before: far_future,
            })
            .unwrap();
        assert_eq!(removed, 1);
        // wing-b is untouched.
        let remaining = b.list_drawers(Some("wing-b"), 10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].source_id, "b");
    }

    #[test]
    fn get_returns_empty_for_nonexistent_ids() {
        let b = InMemoryBackend::new();
        let result = b.get(&[999, 1000]).unwrap();
        assert!(result.drawers.is_empty());
    }

    #[test]
    fn get_multiple_ids() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        let ids = b
            .add(&[
                ins(&e, "1", "alpha", "d"),
                ins(&e, "2", "beta", "d"),
                ins(&e, "3", "gamma", "d"),
            ])
            .unwrap();
        assert_eq!(ids.len(), 3);
        let result = b.get(&ids).unwrap();
        assert_eq!(result.drawers.len(), 3);
        let bodies: std::collections::HashSet<&str> =
            result.drawers.iter().map(|d| d.body.as_str()).collect();
        assert!(bodies.contains("alpha"));
        assert!(bodies.contains("beta"));
        assert!(bodies.contains("gamma"));
    }

    #[test]
    fn list_drawers_limit_zero_returns_all() {
        let e = HashEmbedder::new();
        let b = InMemoryBackend::new();
        for i in 0..5 {
            b.add(&[ins(&e, &i.to_string(), &format!("body {i}"), "d")])
                .unwrap();
        }
        // limit == 0 means unlimited.
        let all = b.list_drawers(None, 0).unwrap();
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn vacuum_is_noop_for_in_memory() {
        let b = InMemoryBackend::new();
        // vacuum must not error on the in-memory backend.
        b.vacuum().unwrap();
    }

    #[test]
    fn health_is_ok_for_in_memory() {
        let b = InMemoryBackend::new();
        assert_eq!(b.health(), crate::types::HealthStatus::Ok);
    }
}
