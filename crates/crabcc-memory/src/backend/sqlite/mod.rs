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

mod encoding;
mod ensure;

use crate::backend::{cosine, Backend, LexicalQuery};
use crate::types::*;
use anyhow::{anyhow, Context, Result};
use crabcc_core::hash::sha256_hex;
use encoding::{blob_to_vec, fts_match_string, now_secs, vec_to_blob};
#[cfg(feature = "memory-vec")]
use ensure::register_sqlite_vec_once;
use ensure::{ensure_room, ensure_wing};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

const SCHEMA: &str = include_str!("../../../schema/001_init.sql");

pub struct SqliteBackend {
    conn: Mutex<Connection>,
    /// Optional FSST codec, loaded from `<db_dir>/fsst.symbols` at open time.
    /// Same on-disk file used by `crabcc-core` for symbol-store compression —
    /// no second symbol table to maintain. When `Some`, drawer bodies are
    /// encoded on insert and decoded transparently on read.
    #[cfg(feature = "compress")]
    codec: Option<crabcc_core::compress::Codec>,
}

// `Send` trivially via Mutex<Connection>; rusqlite's Connection is Send.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<SqliteBackend>();
};

impl SqliteBackend {
    pub fn open(path: &Path) -> Result<Self> {
        #[cfg(feature = "memory-vec")]
        register_sqlite_vec_once();

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

        // Idempotent additive migration: pre-existing M0 databases lack the
        // `body_enc` column. Probe via pragma_table_info and ALTER if missing.
        // Same pattern as crabcc_core::store::Store::open uses for
        // symbols.signature_enc — keeps old indexes readable without forcing
        // a rebuild step.
        let has_enc: bool = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info('drawers') WHERE name = 'body_enc'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !has_enc {
            conn.execute(
                "ALTER TABLE drawers ADD COLUMN body_enc INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }

        // v2.5.3 (#19) — `drawer_embeddings.embedding_model` + `embedded_at`.
        // Prep for sqlite-vec (M0.5, #17) and fastembed-rs (M1, #18) cohabiting
        // with the M0 hash embedder. Defaults map old rows to 'hash-m0' / 0,
        // which is correct: every pre-2.5.3 vector came from `HashEmbedder`.
        let has_emb_model: bool = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info('drawer_embeddings') WHERE name = 'embedding_model'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !has_emb_model {
            conn.execute(
                "ALTER TABLE drawer_embeddings ADD COLUMN embedding_model TEXT NOT NULL DEFAULT 'hash-m0'",
                [],
            )?;
        }
        let has_emb_at: bool = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info('drawer_embeddings') WHERE name = 'embedded_at'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !has_emb_at {
            conn.execute(
                "ALTER TABLE drawer_embeddings ADD COLUMN embedded_at INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }

        // FTS5 backfill — `drawers_fts` is a CREATE-IF-NOT-EXISTS virtual
        // table, so v2.1 / v2.2.1 databases (no FTS at write time) will own an
        // empty index after the first upgraded open. If drawers > 0 but FTS
        // rows == 0 the lexical path would silently return zero hits forever;
        // do a single pass to populate. We need plaintext bodies, so we
        // route through `body_from_row` (same decoder query() uses).
        let drawer_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM drawers", [], |r| r.get(0))
            .unwrap_or(0);
        let fts_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM drawers_fts", [], |r| r.get(0))
            .unwrap_or(0);
        if drawer_count > 0 && fts_count == 0 {
            // Open codec early just for the backfill — needs the same
            // decode path that `body_from_row` uses. We can't call
            // `body_from_row` yet (no `Self` until after this block), so
            // inline the equivalent decode here.
            #[cfg(feature = "compress")]
            let pre_codec = {
                let symbols_path = path
                    .parent()
                    .map(|p| p.join("fsst.symbols"))
                    .unwrap_or_else(|| std::path::PathBuf::from("fsst.symbols"));
                if symbols_path.exists() {
                    crabcc_core::compress::Codec::load(&symbols_path).ok()
                } else {
                    None
                }
            };
            let mut select = conn.prepare("SELECT id, body, body_enc FROM drawers")?;
            let rows = select
                .query_map([], |r| {
                    let id: i64 = r.get(0)?;
                    let enc: i64 = r.get(2)?;
                    let body: String = if enc == 1 {
                        #[cfg(feature = "compress")]
                        {
                            if let Some(codec) = pre_codec.as_ref() {
                                let blob: Vec<u8> = r.get(1)?;
                                String::from_utf8_lossy(&codec.decompress(&blob)).into_owned()
                            } else {
                                String::new()
                            }
                        }
                        #[cfg(not(feature = "compress"))]
                        {
                            String::new()
                        }
                    } else {
                        r.get(1)?
                    };
                    Ok((id, body))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            // Drop the prepared statement before issuing inserts to release
            // the connection's borrow.
            drop(select);
            for (id, body) in rows {
                conn.execute(
                    "INSERT INTO drawers_fts(rowid, body) VALUES (?1, ?2)",
                    params![id, body],
                )?;
            }
        }

        let _ = conn.execute_batch("PRAGMA optimize;");

        // v2.5.1 (#17) — sqlite-vec virtual table for ANN search. Empty
        // until #20 wires the search path. Dim is fixed at 384 to match
        // MiniLM-L6-v2 (M1 default in #18). M0 hash embeddings continue to
        // live in `drawer_embeddings.bytes`; only M1+ vectors will land in
        // this table. `IF NOT EXISTS` makes re-open idempotent.
        #[cfg(feature = "memory-vec")]
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS drawers_vec USING vec0(
                drawer_id INTEGER PRIMARY KEY,
                embedding FLOAT[384]
             );",
        )
        .context("create drawers_vec virtual table")?;

        // Codec discovery — sibling `fsst.symbols` next to the DB. Same shape
        // as the symbol-store's discovery so a single `.crabcc/fsst.symbols`
        // file serves both stores when they live in the same directory.
        #[cfg(feature = "compress")]
        let codec = {
            let symbols_path = path
                .parent()
                .map(|p| p.join("fsst.symbols"))
                .unwrap_or_else(|| std::path::PathBuf::from("fsst.symbols"));
            if symbols_path.exists() {
                Some(
                    crabcc_core::compress::Codec::load(&symbols_path)
                        .context("load fsst symbol table for memory backend")?,
                )
            } else {
                None
            }
        };

        #[cfg(feature = "compress")]
        {
            Ok(Self {
                conn: Mutex::new(conn),
                codec,
            })
        }
        #[cfg(not(feature = "compress"))]
        {
            Ok(Self {
                conn: Mutex::new(conn),
            })
        }
    }

    /// Test-only accessor: did we pick up an FSST symbol table at open?
    #[cfg(feature = "compress")]
    pub fn has_codec(&self) -> bool {
        self.codec.is_some()
    }

    /// Decode a row's `body` column, honoring `body_enc`. Centralized so
    /// `query()` and `get()` share identical semantics — the alternative was
    /// copy-pasting the same branch into every callback. Mirrors the helper
    /// `crabcc_core::store::Store::signature_from_row`.
    fn body_from_row(
        &self,
        row: &rusqlite::Row,
        body_idx: usize,
        enc_idx: usize,
    ) -> rusqlite::Result<String> {
        let enc: i64 = row.get(enc_idx)?;
        // Default-features build (compress on): try to decode if enc=1.
        // No-default-features build: enc must be 0 in any DB we see.
        #[cfg(feature = "compress")]
        if enc == 1 {
            if let Some(codec) = self.codec.as_ref() {
                let blob: Vec<u8> = row.get(body_idx)?;
                let plain = codec.decompress(&blob);
                return Ok(String::from_utf8_lossy(&plain).into_owned());
            } else {
                // Encoded row but no codec loaded → return empty string. The
                // bytes stay correct on disk; rebuilding the codec restores
                // readability. We don't error so callers walking a query
                // result aren't blocked by a single unreadable row.
                return Ok(String::new());
            }
        }
        let _ = enc;
        row.get::<_, String>(body_idx)
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

            // Auto-upsert any referenced session so the drawer's FK resolves.
            // Caller may later enrich metadata via SqliteBackend::upsert_session.
            if let Some(sid) = &d.session_id {
                tx.execute(
                    "INSERT OR IGNORE INTO sessions(id, started_at) VALUES (?1, ?2)",
                    params![sid, now],
                )?;
            }

            let wing_id = ensure_wing(&tx, &d.wing)?;
            let room_id = match d.room.as_deref() {
                Some(r) => Some(ensure_room(&tx, wing_id, r)?),
                None => None,
            };
            // SQLite type-affinity: BLOB stored in a TEXT column. `body_enc=1`
            // is the source of truth — we never inspect the column type at
            // read time, only the enc flag.
            #[cfg(feature = "compress")]
            let inserted = if let Some(codec) = self.codec.as_ref() {
                let encoded = codec.compress(d.body.as_bytes());
                tx.execute(
                    "INSERT INTO drawers(wing_id, room_id, session_id, source_id, body, created_at, sha256, body_enc)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)",
                    params![wing_id, room_id, d.session_id, d.source_id, encoded, now, sha],
                )?;
                true
            } else {
                false
            };
            #[cfg(not(feature = "compress"))]
            let inserted = false;
            if !inserted {
                tx.execute(
                    "INSERT INTO drawers(wing_id, room_id, session_id, source_id, body, created_at, sha256, body_enc)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                    params![wing_id, room_id, d.session_id, d.source_id, d.body, now, sha],
                )?;
            }
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO drawer_embeddings(drawer_id, dim, bytes) VALUES (?1, ?2, ?3)",
                params![id, d.embedding.len() as i64, vec_to_blob(&d.embedding)],
            )?;
            // FTS5 index uses the drawer's id as rowid so KNN ids and BM25
            // ids share the same namespace — RRF fusion in Palace blends
            // by drawer id directly. Plaintext body is indexed regardless
            // of body_enc so BM25 sees real terms even on FSST-encoded rows.
            tx.execute(
                "INSERT INTO drawers_fts(rowid, body) VALUES (?1, ?2)",
                params![id, d.body],
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
            "SELECT d.id, d.body, d.body_enc, d.source_id, w.name AS wing, r.name AS room, e.bytes
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
            let body = self.body_from_row(row, 1, 2)?;
            let source: String = row.get(3)?;
            let wing: String = row.get(4)?;
            let room: Option<String> = row.get(5)?;
            let bytes: Vec<u8> = row.get(6)?;
            Ok((id, body, source, wing, room, bytes))
        })?;

        // `scored` accumulates one entry per matching drawer row before
        // sorting + truncating to `q.limit`. The eventual ceiling is the
        // total drawer count (unknown here without an extra COUNT), so
        // the capacity hint is a heuristic: `q.limit.max(64)` skips the
        // early 0 → 4 → 8 → 16 → 32 → 64 doubling chain in the common
        // case. Larger result sets still pay re-allocs, but those are
        // amortised under the cosine compute on each row.
        let mut scored: Vec<(f32, DrawerHit)> = Vec::with_capacity(q.limit.max(64));
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
            "SELECT d.id, w.name, r.name, d.session_id, d.source_id, d.body, d.body_enc, d.sha256, d.created_at
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
                body: self.body_from_row(row, 5, 6)?,
                sha256: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?;
        let drawers: rusqlite::Result<Vec<Drawer>> = rows.collect();
        Ok(GetResult { drawers: drawers? })
    }

    fn delete(&self, sel: &DeleteSel) -> Result<usize> {
        let mut conn = self.conn.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        // FTS5 isn't a regular table — FK CASCADE on drawers does NOT
        // propagate to drawers_fts. We resolve the affected ids first,
        // delete from drawers (which cascades drawer_embeddings), then
        // emit one FTS delete per id under the same transaction so the
        // pair is atomic.
        let tx = conn.transaction()?;
        let ids: Vec<DrawerId> = match sel {
            DeleteSel::All => {
                let mut stmt = tx.prepare("SELECT id FROM drawers")?;
                let rows = stmt.query_map([], |r| r.get::<_, DrawerId>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            }
            DeleteSel::ById(want) => {
                if want.is_empty() {
                    Vec::new()
                } else {
                    let placeholders = vec!["?"; want.len()].join(",");
                    let sql = format!("SELECT id FROM drawers WHERE id IN ({})", placeholders);
                    let id_vals: Vec<rusqlite::types::Value> =
                        want.iter().map(|i| (*i).into()).collect();
                    let id_refs: Vec<&dyn rusqlite::ToSql> =
                        id_vals.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
                    let mut stmt = tx.prepare(&sql)?;
                    let rows = stmt.query_map(id_refs.as_slice(), |r| r.get::<_, DrawerId>(0))?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                }
            }
            DeleteSel::BySource(src) => {
                let mut stmt = tx.prepare("SELECT id FROM drawers WHERE source_id = ?1")?;
                let rows = stmt.query_map(params![src], |r| r.get::<_, DrawerId>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            }
            DeleteSel::BeforeInWing { wing, before } => {
                let mut stmt = tx.prepare(
                    "SELECT d.id FROM drawers d
                     JOIN wings w ON w.id = d.wing_id
                     WHERE w.name = ?1 AND d.created_at < ?2",
                )?;
                let rows = stmt.query_map(params![wing, before], |r| r.get::<_, DrawerId>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        let n = match sel {
            DeleteSel::All => tx.execute("DELETE FROM drawers", [])?,
            DeleteSel::ById(want) => {
                if want.is_empty() {
                    0
                } else {
                    let placeholders = vec!["?"; want.len()].join(",");
                    let sql = format!("DELETE FROM drawers WHERE id IN ({})", placeholders);
                    let id_vals: Vec<rusqlite::types::Value> =
                        want.iter().map(|i| (*i).into()).collect();
                    let id_refs: Vec<&dyn rusqlite::ToSql> =
                        id_vals.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
                    tx.execute(&sql, id_refs.as_slice())?
                }
            }
            DeleteSel::BySource(src) => {
                tx.execute("DELETE FROM drawers WHERE source_id = ?1", params![src])?
            }
            DeleteSel::BeforeInWing { wing, before } => tx.execute(
                "DELETE FROM drawers
                 WHERE wing_id = (SELECT id FROM wings WHERE name = ?1)
                   AND created_at < ?2",
                params![wing, before],
            )?,
        };
        // FTS5 contentless delete idiom: INSERT a 'delete' command row.
        for id in &ids {
            tx.execute(
                "INSERT INTO drawers_fts(drawers_fts, rowid, body) VALUES('delete', ?1, '')",
                params![id],
            )?;
        }
        tx.commit()?;
        Ok(n)
    }

    fn query_lexical(&self, q: &LexicalQuery) -> Result<QueryResult> {
        let conn = self.conn.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let mut sql = String::from(
            "SELECT d.id, d.body, d.body_enc, d.source_id, w.name AS wing, r.name AS room,
                    bm25(drawers_fts) AS rank
             FROM drawers_fts
             JOIN drawers d ON d.id = drawers_fts.rowid
             JOIN wings w ON w.id = d.wing_id
             LEFT JOIN rooms r ON r.id = d.room_id
             WHERE drawers_fts MATCH ?",
        );
        let mut args: Vec<rusqlite::types::Value> = vec![fts_match_string(&q.text).into()];
        if let Some(w) = &q.wing {
            sql.push_str(" AND w.name = ?");
            args.push(w.clone().into());
        }
        if let Some(r) = &q.room {
            sql.push_str(" AND r.name = ?");
            args.push(r.clone().into());
        }
        sql.push_str(" ORDER BY rank LIMIT ?");
        args.push((q.limit as i64).into());

        let mut stmt = conn.prepare(&sql)?;
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            args.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(params_refs.as_slice(), |row| {
            let id: i64 = row.get(0)?;
            let body = self.body_from_row(row, 1, 2)?;
            let source: String = row.get(3)?;
            let wing: String = row.get(4)?;
            let room: Option<String> = row.get(5)?;
            // bm25() returns negative scores (lower = better). Negate so
            // higher = better, matching cosine's convention. Hybrid fusion
            // only uses ranks (RRF), not absolute values, so the raw scale
            // is fine — we don't fake a [0,1] mapping.
            let bm25: f64 = row.get(6)?;
            let score = (-bm25) as f32;
            Ok(DrawerHit {
                id,
                score,
                source_id: source,
                body,
                wing,
                room,
            })
        })?;
        let hits: rusqlite::Result<Vec<DrawerHit>> = rows.collect();
        Ok(QueryResult { hits: hits? })
    }

    fn count(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM drawers", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    fn vacuum(&self) -> Result<()> {
        // VACUUM cannot run inside a transaction and rewrites the whole
        // DB file, so it's a single-statement, separate call. WAL mode
        // is preserved (sqlite restores pragmas across the rewrite).
        let conn = self.conn.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        conn.execute("VACUUM", [])?;
        Ok(())
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

    fn list_drawers(&self, wing: Option<&str>, limit: usize) -> Result<Vec<Drawer>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("poisoned mutex"))?;
        let mut sql = String::from(
            "SELECT d.id, w.name, r.name, d.session_id, d.source_id, d.body, d.sha256, d.created_at
             FROM drawers d
             JOIN wings w ON w.id = d.wing_id
             LEFT JOIN rooms r ON r.id = d.room_id",
        );
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(w) = wing {
            sql.push_str(" WHERE w.name = ?");
            args.push(w.to_string().into());
        }
        sql.push_str(" ORDER BY d.id ASC");
        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        let mut stmt = conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> =
            args.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(refs.as_slice(), |row| {
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
        let collected: rusqlite::Result<Vec<Drawer>> = rows.collect();
        Ok(collected?)
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

    /// Schema gate: the additive `session_reads` table from the
    /// lean-ctx integration plan must be present in any freshly-
    /// opened db. The `crabcc read` command (#3) writes here, so
    /// open() landing the table is a load-bearing precondition.
    #[test]
    fn open_creates_session_reads_table() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let _ = SqliteBackend::open(&path).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'session_reads'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "session_reads table should be created on open");

        // Smoke-check the column shape — a future schema migration
        // that drops or renames a column would silently break #3.
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(session_reads)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for required in &[
            "path",
            "session_id",
            "mtime_ns",
            "content_hash",
            "served_mode",
            "served_at",
            "bytes_returned",
            "read_count",
        ] {
            assert!(
                cols.iter().any(|c| c == required),
                "session_reads missing column {required}; got {cols:?}"
            );
        }
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

    // v2.5.3 (#19) — drawer_embeddings schema migration tests.

    #[test]
    fn migration_adds_embedding_model_and_embedded_at_to_pre_existing_db() {
        // Pre-2.5.3 dbs have a `drawer_embeddings` table without the
        // `embedding_model` / `embedded_at` columns. SqliteBackend::open
        // must detect the missing columns and ALTER them in.
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE drawer_embeddings (
                    drawer_id INTEGER PRIMARY KEY,
                    dim       INTEGER NOT NULL,
                    bytes     BLOB    NOT NULL
                 );",
            )
            .unwrap();
        }

        let _ = SqliteBackend::open(&path).unwrap();

        let probe = Connection::open(&path).unwrap();
        let cols: Vec<String> = probe
            .prepare("SELECT name FROM pragma_table_info('drawer_embeddings')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            cols.iter().any(|c| c == "embedding_model"),
            "embedding_model column missing after open; got: {cols:?}"
        );
        assert!(
            cols.iter().any(|c| c == "embedded_at"),
            "embedded_at column missing after open; got: {cols:?}"
        );
    }

    #[test]
    fn migration_idempotent_on_repeat_open() {
        // ALTER TABLE ADD COLUMN must not error when re-running on a db that
        // already has the columns. Open three times back-to-back.
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let _ = SqliteBackend::open(&path).unwrap();
        let _ = SqliteBackend::open(&path).unwrap();
        let _ = SqliteBackend::open(&path).unwrap();
    }

    #[test]
    fn embedding_columns_default_for_new_rows() {
        // New inserts (via the M0 add path) don't explicitly set the new
        // columns yet — that's #18's job. Confirm the SQL DEFAULTs land
        // 'hash-m0' / 0 so existing add() callers stay correct.
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let b = SqliteBackend::open(&path).unwrap();
        let e = HashEmbedder::new();
        let id = b.add(&[ins(&e, "src", "body", "wing-d")]).unwrap()[0];

        let probe = Connection::open(&path).unwrap();
        let model: String = probe
            .query_row(
                "SELECT embedding_model FROM drawer_embeddings WHERE drawer_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(model, "hash-m0");
        let at: i64 = probe
            .query_row(
                "SELECT embedded_at FROM drawer_embeddings WHERE drawer_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(at, 0);
    }

    // v2.5.1 (#17) — sqlite-vec extension + drawers_vec virtual table.
    #[cfg(feature = "memory-vec")]
    mod vec_extension {
        use super::*;

        #[test]
        fn vec_version_returns_versioned_string() {
            // Opening any SqliteBackend triggers `register_sqlite_vec_once`,
            // after which every Connection on this process can call
            // `vec_version()`. Round-trip through a sibling connection so
            // we exercise the auto-extension path, not just the inner conn.
            let dir = tempdir().unwrap();
            let path = dir.path().join("memory.db");
            let _ = SqliteBackend::open(&path).unwrap();
            let probe = Connection::open(&path).unwrap();
            let v: String = probe
                .query_row("SELECT vec_version()", [], |r| r.get(0))
                .unwrap();
            assert!(v.starts_with('v'), "expected `v` prefix, got: {v}");
        }

        #[test]
        fn drawers_vec_virtual_table_exists_after_open() {
            // The vec0 virtual table must be created at Backend::open time
            // and visible from any subsequent connection.
            let dir = tempdir().unwrap();
            let path = dir.path().join("memory.db");
            let _ = SqliteBackend::open(&path).unwrap();
            let probe = Connection::open(&path).unwrap();
            let n: i64 = probe
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master \
                     WHERE type = 'table' AND name = 'drawers_vec'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "drawers_vec virtual table missing");
        }

        #[test]
        fn drawers_vec_creation_idempotent_on_repeat_open() {
            // CREATE VIRTUAL TABLE IF NOT EXISTS must not error on the 2nd /
            // 3rd open of the same db. Mirrors the existing migration tests.
            let dir = tempdir().unwrap();
            let path = dir.path().join("memory.db");
            let _ = SqliteBackend::open(&path).unwrap();
            let _ = SqliteBackend::open(&path).unwrap();
            let _ = SqliteBackend::open(&path).unwrap();
        }
    }

    #[cfg(feature = "compress")]
    mod fsst {
        use super::*;
        use crabcc_core::compress::Codec;

        fn train_codec_for_drawers() -> Codec {
            // Drawer-shaped samples: research §3.1 projects 1.4–3.1× on
            // Claude-Code-style transcripts ("I'll help you", code excerpts).
            let owned: Vec<String> = (0..400)
                .map(|i| {
                    format!(
                        "I'll help you with that. Let me check {}_{}.\nfunction handler_{}(input: string): Promise<Result> {{\n  return new Promise((resolve) => resolve({{ ok: true, id: {} }}));\n}}\nThis returns a Result with the id field set.",
                        ["loadUser", "saveOrder", "fetchHits", "indexFile"][i % 4],
                        i,
                        i,
                        i,
                    )
                })
                .collect();
            let refs: Vec<&[u8]> = owned.iter().map(|s| s.as_bytes()).collect();
            Codec::train(&refs).unwrap()
        }

        #[test]
        fn add_query_get_round_trip_with_codec() {
            let dir = tempdir().unwrap();
            let db_path = dir.path().join("memory.db");
            let symbols_path = dir.path().join("fsst.symbols");

            train_codec_for_drawers().save(&symbols_path).unwrap();

            let b = SqliteBackend::open(&db_path).unwrap();
            assert!(
                b.has_codec(),
                "codec must load when fsst.symbols sits next to memory.db"
            );

            let e = HashEmbedder::new();
            let bodies = [
                "I'll help you with that. Let me check loadUser_42.\nfunction handler_42(input: string): Promise<Result> { … }",
                "Short note.",
                "Multi-line\nbody with\nseveral\nlines and a unicode burst: 🦀 → 한국어 → mañana",
            ];
            let inserts: Vec<DrawerInsert> = bodies
                .iter()
                .enumerate()
                .map(|(i, body)| DrawerInsert {
                    wing: "drawers-wing".into(),
                    room: None,
                    source_id: format!("src-{i}"),
                    body: (*body).to_string(),
                    embedding: e.embed_one(body).unwrap(),
                    session_id: None,
                })
                .collect();
            let ids = b.add(&inserts).unwrap();
            assert_eq!(ids.len(), bodies.len());

            // get() returns each body byte-identical to what we inserted.
            let got = b.get(&ids).unwrap();
            assert_eq!(got.drawers.len(), bodies.len());
            for (i, drawer) in got.drawers.iter().enumerate() {
                let expected_idx = inserts
                    .iter()
                    .position(|d| d.source_id == drawer.source_id)
                    .unwrap();
                assert_eq!(
                    drawer.body, bodies[expected_idx],
                    "drawer {i} (src {}) round-trip mismatch",
                    drawer.source_id
                );
            }

            // query() also returns plaintext bodies after decode.
            let q = Query {
                embedding: e.embed_one(bodies[0]).unwrap(),
                limit: bodies.len(),
                wing: None,
                room: None,
            };
            let r = b.query(&q).unwrap();
            assert_eq!(r.hits.len(), bodies.len());
            let returned: std::collections::HashSet<&str> =
                r.hits.iter().map(|h| h.body.as_str()).collect();
            for body in bodies {
                assert!(returned.contains(body), "query() did not return: {body}");
            }
        }

        #[test]
        fn dedup_uses_plaintext_sha_even_when_encoded() {
            // sha256 must be computed on the plaintext body, not the encoded
            // bytes — otherwise the same drawer re-inserted after a codec
            // change would produce a different sha and break dedup.
            let dir = tempdir().unwrap();
            let db_path = dir.path().join("memory.db");
            let symbols_path = dir.path().join("fsst.symbols");
            train_codec_for_drawers().save(&symbols_path).unwrap();

            let b = SqliteBackend::open(&db_path).unwrap();
            let e = HashEmbedder::new();
            let body = "deduplication-target body content";
            let mk = || DrawerInsert {
                wing: "w".into(),
                room: None,
                source_id: "same-source".into(),
                body: body.into(),
                embedding: e.embed_one(body).unwrap(),
                session_id: None,
            };
            let id1 = b.add(&[mk()]).unwrap()[0];
            let id2 = b.add(&[mk()]).unwrap()[0];
            assert_eq!(
                id1, id2,
                "second insert with identical (source, plaintext) must dedup"
            );
            assert_eq!(b.count().unwrap(), 1);
        }

        #[test]
        fn open_migrates_pre_existing_db_without_body_enc() {
            // Simulate a v1 memory.db produced before body_enc landed:
            // create the table by hand WITHOUT the column, then open via
            // SqliteBackend and confirm it ALTERs idempotently.
            let dir = tempdir().unwrap();
            let path = dir.path().join("memory.db");
            {
                let conn = Connection::open(&path).unwrap();
                conn.execute_batch(
                    "CREATE TABLE wings (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
                                          kind TEXT NOT NULL, created_at INTEGER NOT NULL);
                     CREATE TABLE drawers (
                         id          INTEGER PRIMARY KEY,
                         wing_id     INTEGER NOT NULL REFERENCES wings(id) ON DELETE CASCADE,
                         room_id     INTEGER,
                         session_id  TEXT,
                         source_id   TEXT NOT NULL,
                         body        TEXT NOT NULL,
                         created_at  INTEGER NOT NULL,
                         sha256      TEXT NOT NULL,
                         UNIQUE(source_id, sha256)
                     );",
                )
                .unwrap();
                let has_enc: bool = conn
                    .query_row(
                        "SELECT 1 FROM pragma_table_info('drawers') WHERE name = 'body_enc'",
                        [],
                        |_| Ok(true),
                    )
                    .optional()
                    .unwrap()
                    .unwrap_or(false);
                assert!(!has_enc, "test fixture should lack body_enc to begin with");
            }
            // Open via SqliteBackend — schema apply is idempotent (CREATE
            // IF NOT EXISTS) and our ALTER probe adds the column.
            let _b = SqliteBackend::open(&path).unwrap();
            let probe = Connection::open(&path).unwrap();
            let has_enc: bool = probe
                .query_row(
                    "SELECT 1 FROM pragma_table_info('drawers') WHERE name = 'body_enc'",
                    [],
                    |_| Ok(true),
                )
                .optional()
                .unwrap()
                .unwrap_or(false);
            assert!(has_enc, "Store::open must add body_enc to pre-existing DBs");
        }
    }

    #[test]
    fn add_auto_upserts_referenced_session() {
        // The FK on drawers.session_id rejects rows whose session_id has no
        // matching sessions(id) row. SqliteBackend::add must auto-INSERT OR
        // IGNORE the session so callers (e.g., CLI auto_capture using
        // $TERM_SESSION_ID) don't have to upsert the session first.
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let b = SqliteBackend::open(&path).unwrap();
        let e = HashEmbedder::new();

        let mut d = ins(&e, "doc:1", "body", "default");
        d.session_id = Some("term:fresh".into());
        b.add(&[d]).unwrap();

        let probe = Connection::open(&path).unwrap();
        let n: i64 = probe
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = 'term:fresh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "session row should be auto-created");
    }

    #[test]
    fn add_does_not_create_session_when_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let b = SqliteBackend::open(&path).unwrap();
        let e = HashEmbedder::new();

        // session_id=None — sessions table must stay empty.
        b.add(&[ins(&e, "doc:1", "body", "default")]).unwrap();

        let probe = Connection::open(&path).unwrap();
        let n: i64 = probe
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn add_auto_upsert_does_not_overwrite_existing_session() {
        // First explicit upsert with full metadata, then add() with same id.
        // INSERT OR IGNORE in add() must NOT overwrite the existing metadata.
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let b = SqliteBackend::open(&path).unwrap();
        let e = HashEmbedder::new();

        b.upsert_session(&Session {
            id: "term:rich".into(),
            started_at: 100,
            cwd: Some("/somewhere".into()),
            git_branch: Some("feature".into()),
            git_sha: Some("c0ffee".into()),
        })
        .unwrap();

        let mut d = ins(&e, "doc:1", "body", "default");
        d.session_id = Some("term:rich".into());
        b.add(&[d]).unwrap();

        let probe = Connection::open(&path).unwrap();
        let (cwd, branch): (Option<String>, Option<String>) = probe
            .query_row(
                "SELECT cwd, git_branch FROM sessions WHERE id = 'term:rich'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(cwd.as_deref(), Some("/somewhere"));
        assert_eq!(branch.as_deref(), Some("feature"));
    }

    #[test]
    fn list_drawers_includes_session_id() {
        let dir = tempdir().unwrap();
        let b = SqliteBackend::open(&dir.path().join("memory.db")).unwrap();
        let e = HashEmbedder::new();
        let mut d = ins(&e, "doc:1", "body", "default");
        d.session_id = Some("term:list".into());
        b.add(&[d]).unwrap();
        let listed = b.list_drawers(None, 10).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id.as_deref(), Some("term:list"));
    }

    #[test]
    fn session_id_persists_across_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let e = HashEmbedder::new();
        {
            let b = SqliteBackend::open(&path).unwrap();
            let mut d = ins(&e, "doc:1", "body", "default");
            d.session_id = Some("term:durable".into());
            b.add(&[d]).unwrap();
        }
        {
            let b = SqliteBackend::open(&path).unwrap();
            let listed = b.list_drawers(None, 10).unwrap();
            assert_eq!(listed[0].session_id.as_deref(), Some("term:durable"));
        }
    }

    #[test]
    fn list_drawers_filters_by_wing_only_not_session() {
        // Confirm list_drawers' wing filter is independent of session — two
        // drawers in same session but different wings get split correctly.
        let dir = tempdir().unwrap();
        let b = SqliteBackend::open(&dir.path().join("memory.db")).unwrap();
        let e = HashEmbedder::new();
        let mut d1 = ins(&e, "1", "alpha", "wing-a");
        d1.session_id = Some("s1".into());
        let mut d2 = ins(&e, "2", "beta", "wing-b");
        d2.session_id = Some("s1".into());
        b.add(&[d1, d2]).unwrap();
        let a = b.list_drawers(Some("wing-a"), 10).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].session_id.as_deref(), Some("s1"));
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

    // ---------- M1: FTS5 lexical search + backfill ----------

    #[test]
    fn lexical_query_returns_keyword_matches() {
        let dir = tempdir().unwrap();
        let b = SqliteBackend::open(&dir.path().join("memory.db")).unwrap();
        let e = HashEmbedder::new();
        b.add(&[
            ins(&e, "1", "the quick brown fox", "default"),
            ins(&e, "2", "lazy cat sleeps", "default"),
            ins(&e, "3", "fox in henhouse", "default"),
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
        let ids: std::collections::HashSet<&str> =
            r.hits.iter().map(|h| h.source_id.as_str()).collect();
        assert_eq!(r.hits.len(), 2);
        assert!(ids.contains("1"));
        assert!(ids.contains("3"));
    }

    #[test]
    fn lexical_empty_query_is_safe() {
        // Empty/whitespace-only queries must NOT raise an FTS5 syntax
        // error — the helper substitutes a never-matching token.
        let dir = tempdir().unwrap();
        let b = SqliteBackend::open(&dir.path().join("memory.db")).unwrap();
        let e = HashEmbedder::new();
        b.add(&[ins(&e, "1", "anything", "default")]).unwrap();
        let r = b
            .query_lexical(&LexicalQuery {
                text: "  ".into(),
                limit: 5,
                wing: None,
                room: None,
            })
            .unwrap();
        assert!(r.hits.is_empty());
    }

    #[test]
    fn lexical_query_handles_apostrophes_and_quotes() {
        // User input with quotes / apostrophes would crash naive FTS5 query
        // building. Confirm sanitised match string survives.
        let dir = tempdir().unwrap();
        let b = SqliteBackend::open(&dir.path().join("memory.db")).unwrap();
        let e = HashEmbedder::new();
        b.add(&[ins(&e, "1", "don't worry about it", "default")])
            .unwrap();
        let r = b
            .query_lexical(&LexicalQuery {
                text: r#"don"t "worry""#.into(),
                limit: 5,
                wing: None,
                room: None,
            })
            .unwrap();
        assert!(!r.hits.is_empty());
    }

    #[test]
    fn delete_drawer_drops_fts_row() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let b = SqliteBackend::open(&path).unwrap();
        let e = HashEmbedder::new();
        let id = b.add(&[ins(&e, "x", "fox jumps", "d")]).unwrap()[0];
        // FTS hit before delete.
        let r = b
            .query_lexical(&LexicalQuery {
                text: "fox".into(),
                limit: 5,
                wing: None,
                room: None,
            })
            .unwrap();
        assert_eq!(r.hits.len(), 1);
        b.delete(&DeleteSel::ById(vec![id])).unwrap();
        // FTS hit must be gone.
        let r = b
            .query_lexical(&LexicalQuery {
                text: "fox".into(),
                limit: 5,
                wing: None,
                room: None,
            })
            .unwrap();
        assert!(r.hits.is_empty());
    }

    #[test]
    fn fts_backfills_for_pre_m1_database() {
        // Simulate a v2.1 memory.db: drawers + drawer_embeddings populated
        // before drawers_fts existed. Open via SqliteBackend (which adds
        // the FTS table via CREATE-IF-NOT-EXISTS) and confirm the backfill
        // populates the index so lexical search works.
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        // Step 1 — write some drawers, then DROP the FTS table to mimic
        // a database created before M1 added FTS5.
        {
            let b = SqliteBackend::open(&path).unwrap();
            let e = HashEmbedder::new();
            b.add(&[
                ins(&e, "1", "alpha beta", "default"),
                ins(&e, "2", "gamma delta", "default"),
            ])
            .unwrap();
        }
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute("DROP TABLE drawers_fts", []).unwrap();
        }
        // Step 2 — reopen. CREATE IF NOT EXISTS recreates the table; the
        // backfill populates rows from existing drawers.
        let b = SqliteBackend::open(&path).unwrap();
        let r = b
            .query_lexical(&LexicalQuery {
                text: "gamma".into(),
                limit: 5,
                wing: None,
                room: None,
            })
            .unwrap();
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].source_id, "2");
    }

    #[test]
    fn fts_backfill_is_idempotent_on_reopen() {
        // Reopening an already-populated DB must NOT double-insert into FTS
        // (the backfill check is "drawer_count > 0 && fts_count == 0").
        let dir = tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let e = HashEmbedder::new();
        {
            let b = SqliteBackend::open(&path).unwrap();
            b.add(&[ins(&e, "1", "alpha beta", "default")]).unwrap();
        }
        // Reopen 5×; FTS rowcount must stay at 1.
        for _ in 0..5 {
            let _ = SqliteBackend::open(&path).unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM drawers_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
