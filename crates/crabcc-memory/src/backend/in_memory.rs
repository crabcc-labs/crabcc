use crate::backend::{cosine, Backend};
use crate::types::*;
use anyhow::{anyhow, Result};
use crabcc_core::hash::sha256_hex;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct Inner {
    next_id: i64,
    rows: HashMap<DrawerId, Stored>,
    sha_index: HashMap<(String, String), DrawerId>,
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
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
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
}
