//! File-backed `Backend` — pure rusqlite, brute-force cosine over an
//! `f32` blob column. Default at M0.
//!
//! Connection setup mirrors `crabcc_core::store::Store::open` (WAL, mmap,
//! busy_timeout) so `.crabcc/memory.db` and `.crabcc/index.db` share
//! durability/perf characteristics.
//!
//! Query path: full table scan filtered by wing/room → cosine in Rust →
//! top-K. Fine for ≤ ~10k drawers (M0 scale target). M0.5 swaps the inner
//! query for a `sqlite-vec` virtual table `MATCH` while keeping the same
//! schema and `Backend` impl signature.
//!
//! Insert path: idempotent on `(source_id, sha256(body))` via a UNIQUE
//! constraint — re-adding an unchanged drawer returns its existing id.
//! Wing/room rows are auto-created within the same transaction.

use crate::backend::{cosine, Backend};
use crate::types::*;
use anyhow::{anyhow, Context, Result};
use crabcc_core::hash::sha256_hex;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;

const SCHEMA: &str = include_str!("../../schema/001_init.sql");

pub struct SqliteBackend {
    conn: Mutex<Connection>,
}

// `Send` trivially via Mutex<Connection>; rusqlite's Connection is Send.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<SqliteBackend>();
};

impl SqliteBackend {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).context("open memory.db")?;
        // Mirrors crabcc_core::store::Store::open. Documented there; keeping
        // the same set of pragmas so a single `.crabcc/` directory has
        // consistent durability/perf characteristics across stores.
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .context("synchronous")?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .context("foreign_keys")?;
        conn.pragma_update(None, "mmap_size", 30_000_000_000_i64)
            .ok();
        conn.pragma_update(None, "temp_store", "MEMORY").ok();
        conn.pragma_update(None, "cache_size", -16_000_i64).ok();
        conn.busy_timeout(std::time::Duration::from_millis(2_000))?;
        conn.execute_batch(SCHEMA).context("apply memory schema")?;
        let _ = conn.execute_batch("PRAGMA optimize;");
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn analyze(&self) -> Result<()> {
        self.conn
            .lock()
            .map_err(|_| anyhow!("poisoned mutex"))?
            .execute_batch("ANALYZE;")
            .context("ANALYZE")?;
        Ok(())
    }

    pub fn upsert_session(&self, s: &Session) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        conn.execute(
            "INSERT INTO sessions(id, started_at, cwd, git_branch, git_sha)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET cwd=excluded.cwd,
                                            git_branch=excluded.git_branch,
                                            git_sha=excluded.git_sha",
            params![s.id, s.started_at, s.cwd, s.git_branch, s.git_sha],
        )?;
        Ok(())
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn ensure_wing(conn: &Connection, name: &str) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO wings(name, kind, created_at) VALUES (?1, 'project', ?2)",
        params![name, now_secs()],
    )?;
    let id: i64 = conn.query_row("SELECT id FROM wings WHERE name = ?1", params![name], |r| {
        r.get(0)
    })?;
    Ok(id)
}

fn ensure_room(conn: &Connection, wing_id: i64, name: &str) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO rooms(wing_id, name) VALUES (?1, ?2)",
        params![wing_id, name],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM rooms WHERE wing_id = ?1 AND name = ?2",
        params![wing_id, name],
        |r| r.get(0),
    )?;
    Ok(id)
}

impl Backend for SqliteBackend {
    fn add(&self, drawers: &[DrawerInsert]) -> Result<Vec<DrawerId>> {
        let mut conn = self.conn.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let tx = conn.transaction()?;
        let mut ids = Vec::with_capacity(drawers.len());
        let now = now_secs();
        for d in drawers {
            let sha = sha256_hex(d.body.as_bytes());

            // Existing (source_id, sha256) → return its id, idempotent.
            let existing: Option<DrawerId> = tx
                .query_row(
                    "SELECT id FROM drawers WHERE source_id = ?1 AND sha256 = ?2",
                    params![d.source_id, sha],
                    |r| r.get(0),
                )
                .ok();
            if let Some(id) = existing {
                ids.push(id);
                continue;
            }

            let wing_id = ensure_wing(&tx, &d.wing)?;
            let room_id = match d.room.as_deref() {
                Some(r) => Some(ensure_room(&tx, wing_id, r)?),
                None => None,
            };
            tx.execute(
                "INSERT INTO drawers(wing_id, room_id, session_id, source_id, body, created_at, sha256)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![wing_id, room_id, d.session_id, d.source_id, d.body, now, sha],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO drawer_embeddings(drawer_id, dim, bytes) VALUES (?1, ?2, ?3)",
                params![id, d.embedding.len() as i64, vec_to_blob(&d.embedding)],
            )?;
            ids.push(id);
        }
        tx.commit()?;
        Ok(ids)
    }

    fn query(&self, q: &Query) -> Result<QueryResult> {
        let conn = self.conn.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        // Brute force: scan all drawers (filtered by wing/room) + embedding,
        // compute cosine in Rust, top-K by score. Replaced by sqlite-vec MATCH
        // in M0.5.
        let mut sql = String::from(
            "SELECT d.id, d.body, d.source_id, w.name AS wing, r.name AS room, e.bytes
             FROM drawers d
             JOIN wings w ON w.id = d.wing_id
             LEFT JOIN rooms r ON r.id = d.room_id
             JOIN drawer_embeddings e ON e.drawer_id = d.id
             WHERE 1=1",
        );
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(w) = &q.wing {
            sql.push_str(" AND w.name = ?");
            args.push(w.clone().into());
        }
        if let Some(r) = &q.room {
            sql.push_str(" AND r.name = ?");
            args.push(r.clone().into());
        }
        let mut stmt = conn.prepare(&sql)?;
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            args.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(params_refs.as_slice(), |row| {
            let id: i64 = row.get(0)?;
            let body: String = row.get(1)?;
            let source: String = row.get(2)?;
            let wing: String = row.get(3)?;
            let room: Option<String> = row.get(4)?;
            let bytes: Vec<u8> = row.get(5)?;
            Ok((id, body, source, wing, room, bytes))
        })?;

        let mut scored: Vec<(f32, DrawerHit)> = Vec::new();
        for row in rows {
            let (id, body, source, wing, room, bytes) = row?;
            let emb = blob_to_vec(&bytes);
            let score = cosine(&q.embedding, &emb);
            scored.push((
                score,
                DrawerHit {
                    id,
                    score,
                    source_id: source,
                    body,
                    wing,
                    room,
                },
            ));
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(q.limit);
        Ok(QueryResult {
            hits: scored.into_iter().map(|(_, h)| h).collect(),
        })
    }

    fn get(&self, ids: &[DrawerId]) -> Result<GetResult> {
        if ids.is_empty() {
            return Ok(GetResult::default());
        }
        let conn = self.conn.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT d.id, w.name, r.name, d.session_id, d.source_id, d.body, d.sha256, d.created_at
             FROM drawers d
             JOIN wings w ON w.id = d.wing_id
             LEFT JOIN rooms r ON r.id = d.room_id
             WHERE d.id IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)?;
        let id_vals: Vec<rusqlite::types::Value> = ids.iter().map(|i| (*i).into()).collect();
        let id_refs: Vec<&dyn rusqlite::ToSql> =
            id_vals.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(id_refs.as_slice(), |row| {
            Ok(Drawer {
                id: row.get(0)?,
                wing: row.get(1)?,
                room: row.get(2)?,
                session_id: row.get(3)?,
                source_id: row.get(4)?,
                body: row.get(5)?,
                sha256: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        let drawers: rusqlite::Result<Vec<Drawer>> = rows.collect();
        Ok(GetResult { drawers: drawers? })
    }

    fn delete(&self, sel: &DeleteSel) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let n = match sel {
            DeleteSel::All => conn.execute("DELETE FROM drawers", [])?,
            DeleteSel::ById(ids) => {
                if ids.is_empty() {
                    0
                } else {
                    let placeholders = vec!["?"; ids.len()].join(",");
                    let sql = format!("DELETE FROM drawers WHERE id IN ({})", placeholders);
                    let id_vals: Vec<rusqlite::types::Value> =
                        ids.iter().map(|i| (*i).into()).collect();
                    let id_refs: Vec<&dyn rusqlite::ToSql> =
                        id_vals.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
                    conn.execute(&sql, id_refs.as_slice())?
                }
            }
            DeleteSel::BySource(src) => {
                conn.execute("DELETE FROM drawers WHERE source_id = ?1", params![src])?
            }
        };
        Ok(n)
    }

    fn count(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM drawers", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    fn health(&self) -> HealthStatus {
        match self.conn.lock() {
            Ok(c) => match c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0)) {
                Ok(_) => HealthStatus::Ok,
                Err(_) => HealthStatus::Degraded,
            },
            Err(_) => HealthStatus::Down,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::{Embedder, HashEmbedder};
    use tempfile::tempdir;

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
    fn open_creates_db_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let _ = SqliteBackend::open(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn add_query_round_trip() {
        let dir = tempdir().unwrap();
        let b = SqliteBackend::open(&dir.path().join("memory.db")).unwrap();
        let e = HashEmbedder::new();
        b.add(&[
            ins(&e, "1", "fox jumps", "default"),
            ins(&e, "2", "cat sleeps", "default"),
        ])
        .unwrap();
        let q = Query {
            embedding: e.embed_one("fox jumps").unwrap(),
            limit: 5,
            wing: None,
            room: None,
        };
        let r = b.query(&q).unwrap();
        assert_eq!(r.hits.len(), 2);
        assert_eq!(r.hits[0].body, "fox jumps");
        assert!(r.hits[0].score > 0.99);
    }

    #[test]
    fn dedup_across_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let e = HashEmbedder::new();
        {
            let b = SqliteBackend::open(&path).unwrap();
            b.add(&[ins(&e, "a", "same body", "d")]).unwrap();
        }
        {
            let b = SqliteBackend::open(&path).unwrap();
            // Re-insert same drawer; dedup should return existing id.
            b.add(&[ins(&e, "a", "same body", "d")]).unwrap();
            assert_eq!(b.count().unwrap(), 1);
        }
    }

    #[test]
    fn delete_by_id_and_by_source() {
        let dir = tempdir().unwrap();
        let b = SqliteBackend::open(&dir.path().join("memory.db")).unwrap();
        let e = HashEmbedder::new();
        let ids = b
            .add(&[
                ins(&e, "x", "one", "d"),
                ins(&e, "x", "two", "d"),
                ins(&e, "y", "three", "d"),
            ])
            .unwrap();
        assert_eq!(b.delete(&DeleteSel::ById(vec![ids[0]])).unwrap(), 1);
        assert_eq!(b.delete(&DeleteSel::BySource("x".into())).unwrap(), 1);
        assert_eq!(b.count().unwrap(), 1);
    }

    #[test]
    fn session_round_trip() {
        let dir = tempdir().unwrap();
        let b = SqliteBackend::open(&dir.path().join("memory.db")).unwrap();
        let e = HashEmbedder::new();
        b.upsert_session(&Session {
            id: "term:abc".into(),
            started_at: now_secs(),
            cwd: Some("/some/path".into()),
            git_branch: Some("main".into()),
            git_sha: Some("deadbeef".into()),
        })
        .unwrap();
        let mut d = ins(&e, "f", "x", "d");
        d.session_id = Some("term:abc".into());
        let id = b.add(&[d]).unwrap()[0];
        let g = b.get(&[id]).unwrap();
        assert_eq!(g.drawers[0].session_id.as_deref(), Some("term:abc"));
    }

    #[test]
    fn health_ok_on_open_db() {
        let dir = tempdir().unwrap();
        let b = SqliteBackend::open(&dir.path().join("memory.db")).unwrap();
        assert_eq!(b.health(), HealthStatus::Ok);
    }

    #[test]
    fn add_empty_is_noop() {
        let dir = tempdir().unwrap();
        let b = SqliteBackend::open(&dir.path().join("memory.db")).unwrap();
        assert!(b.add(&[]).unwrap().is_empty());
        assert_eq!(b.count().unwrap(), 0);
    }

    #[test]
    fn query_limit_zero_returns_empty() {
        let dir = tempdir().unwrap();
        let b = SqliteBackend::open(&dir.path().join("memory.db")).unwrap();
        let e = HashEmbedder::new();
        b.add(&[ins(&e, "1", "alpha", "d")]).unwrap();
        let q = Query {
            embedding: e.embed_one("alpha").unwrap(),
            limit: 0,
            wing: None,
            room: None,
        };
        assert!(b.query(&q).unwrap().hits.is_empty());
    }

    #[test]
    fn query_with_room_filter() {
        let dir = tempdir().unwrap();
        let b = SqliteBackend::open(&dir.path().join("memory.db")).unwrap();
        let e = HashEmbedder::new();
        let mut a = ins(&e, "1", "alpha", "w");
        a.room = Some("room-a".into());
        let mut beta = ins(&e, "2", "beta", "w");
        beta.room = Some("room-b".into());
        b.add(&[a, beta]).unwrap();
        let q = Query {
            embedding: e.embed_one("alpha").unwrap(),
            limit: 10,
            wing: None,
            room: Some("room-b".into()),
        };
        let r = b.query(&q).unwrap();
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].room.as_deref(), Some("room-b"));
    }

    #[test]
    fn delete_drawer_cascades_embedding_row() {
        // FK CASCADE on drawer_embeddings(drawer_id) — verify the embedding
        // row vanishes when its drawer is deleted. Open a sibling read
        // connection to inspect the embeddings table directly.
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let b = SqliteBackend::open(&path).unwrap();
        let e = HashEmbedder::new();
        let id = b.add(&[ins(&e, "x", "body", "d")]).unwrap()[0];

        let probe = Connection::open(&path).unwrap();
        let before: i64 = probe
            .query_row(
                "SELECT COUNT(*) FROM drawer_embeddings WHERE drawer_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(before, 1);

        b.delete(&DeleteSel::ById(vec![id])).unwrap();

        let after: i64 = probe
            .query_row(
                "SELECT COUNT(*) FROM drawer_embeddings WHERE drawer_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, 0);
    }

    #[test]
    fn delete_wing_cascades_drawers() {
        // FK CASCADE on drawers(wing_id) — deleting a wing wipes its drawers.
        // No public wing-delete API at M0; test via a sibling connection
        // with foreign_keys explicitly ON (default OFF for fresh connections).
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let b = SqliteBackend::open(&path).unwrap();
        let e = HashEmbedder::new();
        b.add(&[
            ins(&e, "1", "one", "wing-a"),
            ins(&e, "2", "two", "wing-a"),
            ins(&e, "3", "three", "wing-b"),
        ])
        .unwrap();
        assert_eq!(b.count().unwrap(), 3);

        let mutator = Connection::open(&path).unwrap();
        mutator.pragma_update(None, "foreign_keys", "ON").unwrap();
        mutator
            .execute("DELETE FROM wings WHERE name = 'wing-a'", [])
            .unwrap();

        // Backend sees the cascade — wing-a's two drawers are gone.
        assert_eq!(b.count().unwrap(), 1);
    }

    #[test]
    fn concurrent_writes_via_arc() {
        // SqliteBackend is Send and uses an inner Mutex<Connection>. Two
        // threads each insert N drawers; final count must equal 2N and all
        // calls must succeed.
        use std::sync::Arc;
        use std::thread;
        let dir = tempdir().unwrap();
        let b = Arc::new(SqliteBackend::open(&dir.path().join("memory.db")).unwrap());
        let mut handles = Vec::new();
        for t in 0..2 {
            let bclone = b.clone();
            handles.push(thread::spawn(move || {
                let e = HashEmbedder::new();
                for i in 0..25 {
                    let body = format!("t{t}-doc{i}");
                    bclone
                        .add(&[DrawerInsert {
                            wing: "default".into(),
                            room: None,
                            source_id: format!("t{t}-{i}"),
                            body: body.clone(),
                            embedding: e.embed_one(&body).unwrap(),
                            session_id: None,
                        }])
                        .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(b.count().unwrap(), 50);
    }

    #[test]
    fn session_upsert_idempotent_updates_metadata() {
        let dir = tempdir().unwrap();
        let b = SqliteBackend::open(&dir.path().join("memory.db")).unwrap();
        b.upsert_session(&Session {
            id: "term:abc".into(),
            started_at: 100,
            cwd: Some("/old".into()),
            git_branch: None,
            git_sha: None,
        })
        .unwrap();
        b.upsert_session(&Session {
            id: "term:abc".into(),
            started_at: 100,
            cwd: Some("/new".into()),
            git_branch: Some("main".into()),
            git_sha: Some("deadbeef".into()),
        })
        .unwrap();
        // Verify only one session row exists with the latest metadata.
        let probe = Connection::open(dir.path().join("memory.db")).unwrap();
        let (cwd, branch): (String, String) = probe
            .query_row(
                "SELECT cwd, git_branch FROM sessions WHERE id = ?1",
                params!["term:abc"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(cwd, "/new");
        assert_eq!(branch, "main");
    }
}
