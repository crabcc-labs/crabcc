use crate::types::{Edge, Symbol, SymbolKind};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

const SCHEMA: &str = include_str!("../../../schema/001_init.sql");

/// SQLite-backed symbol store. `Send` (move across threads safely);
/// **not** `Sync` — wrap in `Mutex<Store>` for shared access. `crabcc watch`
/// does exactly that. Single-writer is fine for our workload; WAL mode
/// keeps readers from blocking writers if anyone wants concurrent reads.
pub struct Store {
    conn: Connection,
    /// Optional FSST codec, loaded from a sibling `fsst.symbols` file at
    /// `Store::open` time. When `Some`, writes encode `signature` and reads
    /// decode it transparently. When `None` (or feature disabled), behavior
    /// is byte-identical to the pre-FSST path.
    #[cfg(feature = "compress")]
    codec: Option<crate::compress::Codec>,
}

// Connection is `Send` since rusqlite 0.20; assert at compile time so a future
// dep change can't silently break our threading story.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Store>();
};

impl Store {
    /// Open the index and (when the `compress` feature is built in) auto-load
    /// the FSST codec at `<index_dir>/fsst.symbols` if present. Equivalent to
    /// `open_with_compress(path, true)`.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_compress(path, true)
    }

    /// Like `open`, but the caller controls whether the FSST codec is loaded.
    /// `compress=false` skips codec discovery entirely — encoded rows on disk
    /// stay correct, but reads of `signature_enc=1` rows return `None` until
    /// the codec is re-enabled. New writes go down the plain path.
    pub fn open_with_compress(path: &Path, compress: bool) -> Result<Self> {
        tracing::debug!(target: "crabcc_core::store", path = %path.display(), compress, "Store::open");
        let conn = Connection::open(path).context("open sqlite")?;
        // WAL = concurrent readers + faster writes. NORMAL sync = "fast but
        // still durable on power loss". foreign_keys ON makes our ON DELETE
        // CASCADE fire. busy_timeout absorbs spurious lock contention during
        // `crabcc watch` refreshes that overlap with reader queries.
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .context("synchronous")?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .context("foreign_keys")?;
        // 30 GB mmap cap (sqlite caps to file size automatically; this is the
        // upper limit for memory-mapped I/O — recommended by sqlite docs for
        // read-heavy workloads).
        conn.pragma_update(None, "mmap_size", 30_000_000_000_i64)
            .ok();
        // Keep temp tables / sort spill in RAM. Our index is small enough that
        // this never bites — biggest temp area we generate is during ANALYZE.
        conn.pragma_update(None, "temp_store", "MEMORY").ok();
        // Default page cache is 2 MB; bump to 64 MB. Negative = KiB.
        // 64 MB comfortably holds the working set of a 13k-file repo's
        // hot indexes (issue #112) — measured ~30% bulk-write speedup
        // and faster cold reads vs the prior 16 MB cap.
        conn.pragma_update(None, "cache_size", -64_000_i64).ok();
        conn.busy_timeout(std::time::Duration::from_millis(2_000))?;
        conn.execute_batch(SCHEMA).context("apply schema")?;
        // Idempotent migration: pre-FSST DBs lack `symbols.signature_enc`. The
        // schema above declares it for new DBs; for older indexes we ALTER
        // TABLE in place. PRAGMA table_info is the standard "does this column
        // exist?" probe — cheap and read-only.
        let has_enc: bool = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info('symbols') WHERE name = 'signature_enc'",
                [],
                |_| Ok(true),
            )
            .optional()
            .unwrap_or(None)
            .is_some();
        if !has_enc {
            conn.execute(
                "ALTER TABLE symbols ADD COLUMN signature_enc INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .context("migrate: add symbols.signature_enc")?;
        }
        // v2.0 edges migration: pre-v2 DBs have INTEGER `src_symbol`; recreate
        // the empty table with TEXT (and the dst_kind composite index).
        migrate_edges_text(&conn).context("migrate edges schema")?;
        // PRAGMA optimize is a no-op until the query planner has stats; it
        // becomes useful after ANALYZE. Run it whenever we open — sqlite
        // makes the call cheap when nothing's changed.
        let _ = conn.execute_batch("PRAGMA optimize;");

        // Codec discovery (FSST). The DB path is typically `.crabcc/index.db`;
        // the symbol table lives next to it as `.crabcc/fsst.symbols`. If the
        // file is absent we run uncompressed — matching default-feature builds.
        // When `compress=false`, skip discovery entirely so the runtime flag
        // (`crabcc --compress=false`) can force plain-text mode even when the
        // symbol table is on disk.
        #[cfg(feature = "compress")]
        let codec = if !compress {
            None
        } else {
            let symbols_path = path
                .parent()
                .map(|p| p.join("fsst.symbols"))
                .unwrap_or_else(|| std::path::PathBuf::from("fsst.symbols"));
            if symbols_path.exists() {
                Some(
                    crate::compress::Codec::load(&symbols_path)
                        .context("load fsst symbol table")?,
                )
            } else {
                None
            }
        };
        #[cfg(not(feature = "compress"))]
        let _ = compress; // silence unused-arg warning when feature is off

        #[cfg(feature = "compress")]
        {
            Ok(Self { conn, codec })
        }
        #[cfg(not(feature = "compress"))]
        {
            Ok(Self { conn })
        }
    }

    /// Test-only accessor: did we pick up an FSST symbol table at open?
    #[cfg(feature = "compress")]
    pub fn has_codec(&self) -> bool {
        self.codec.is_some()
    }

    /// Refresh query-planner statistics. Call after a full reindex if you want
    /// the next batch of queries to hit optimal plans. Cheap (~tens of ms on
    /// a 13k-file index) — skipped automatically if data hasn't changed.
    pub fn analyze(&self) -> Result<()> {
        self.conn.execute_batch("ANALYZE;").context("ANALYZE")?;
        Ok(())
    }

    pub fn upsert_file(&self, path: &str, sha256: &str, mtime: i64, lang: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO files(path, sha256, mtime, lang, indexed_at)
             VALUES (?1, ?2, ?3, ?4, strftime('%s','now'))
             ON CONFLICT(path) DO UPDATE SET sha256=excluded.sha256,
                                             mtime=excluded.mtime,
                                             lang=excluded.lang,
                                             indexed_at=strftime('%s','now')",
            params![path, sha256, mtime, lang],
        )?;
        // last_insert_rowid is NOT updated on the UPSERT conflict path, so
        // we must look up the id after to handle both insert and update.
        let id: i64 = self.conn.query_row(
            "SELECT id FROM files WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    pub fn replace_symbols(&self, file_id: i64, symbols: &[Symbol]) -> Result<()> {
        self.conn
            .execute("DELETE FROM symbols WHERE file_id = ?1", params![file_id])?;
        // We always bind `signature_enc` explicitly so the row reflects the
        // encoding actually used (no reliance on the schema DEFAULT).
        let mut stmt = self.conn.prepare(
            "INSERT INTO symbols(file_id, name, kind, signature, parent, line_start, line_end, visibility, signature_enc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for s in symbols {
            // SQLite type-affinity: BLOB stored in a TEXT column. `signature_enc=1` is the source of truth on encoding.
            #[cfg(feature = "compress")]
            {
                if let (Some(codec), Some(plain)) = (self.codec.as_ref(), s.signature.as_ref()) {
                    let encoded: Vec<u8> = codec.compress(plain.as_bytes());
                    stmt.execute(params![
                        file_id,
                        s.name,
                        kind_str(s.kind),
                        encoded,
                        s.parent,
                        s.line_start,
                        s.line_end,
                        s.visibility,
                        1_i64,
                    ])?;
                    continue;
                }
            }
            // Plain path: no codec loaded, feature disabled, or signature is None.
            stmt.execute(params![
                file_id,
                s.name,
                kind_str(s.kind),
                s.signature,
                s.parent,
                s.line_start,
                s.line_end,
                s.visibility,
                0_i64,
            ])?;
        }
        Ok(())
    }

    /// Replace all edges originating from `file_id` with the supplied set.
    /// Mirror of `replace_symbols` — same all-or-nothing per-file shape so
    /// reindexing a file leaves the edges table consistent.
    pub fn replace_edges(&self, file_id: i64, edges: &[Edge]) -> Result<()> {
        self.conn
            .execute("DELETE FROM edges WHERE src_file_id = ?1", params![file_id])?;
        let mut stmt = self.conn.prepare(
            "INSERT INTO edges(src_file_id, src_symbol, dst_name, kind, line)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for e in edges {
            stmt.execute(params![file_id, e.src_symbol, e.dst_name, e.kind, e.line])?;
        }
        Ok(())
    }

    /// Count populated edges. Used to decide whether to take the SQL caller
    /// path or fall back to ast-grep on a stale (v1.0.0) index.
    pub fn edge_count(&self) -> Result<i64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))?;
        Ok(n)
    }

    /// Pure-SQL caller lookup: find every edge whose `dst_name` is `name` and
    /// `kind = 'call'`, returning {file, line, src_symbol} per hit. Cost is
    /// dominated by the index seek on `idx_edges_dst_kind`.
    pub fn callers_of(&self, name: &str) -> Result<Vec<EdgeHit>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.path, e.line, e.src_symbol
             FROM edges e JOIN files f ON e.src_file_id = f.id
             WHERE e.dst_name = ?1 AND e.kind = 'call'
             ORDER BY f.path, e.line",
        )?;
        let rows = stmt.query_map(params![name], |row| {
            Ok(EdgeHit {
                file: row.get(0)?,
                line: row.get::<_, i64>(1)? as u32,
                src_symbol: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Broader reference lookup: edges where `dst_name = name` and `kind` is
    /// any reference-like kind we currently emit (`call`, `ref`). The schema
    /// reserves `import`, `inherit`, `impl` for future extraction passes; add
    /// them to this filter when the extractor starts emitting them.
    pub fn refs_of(&self, name: &str) -> Result<Vec<EdgeHit>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.path, e.line, e.src_symbol
             FROM edges e JOIN files f ON e.src_file_id = f.id
             WHERE e.dst_name = ?1 AND e.kind IN ('call', 'ref')
             ORDER BY f.path, e.line",
        )?;
        let rows = stmt.query_map(params![name], |row| {
            Ok(EdgeHit {
                file: row.get(0)?,
                line: row.get::<_, i64>(1)? as u32,
                src_symbol: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Stream every (src_symbol, dst_name) pair where both ends are populated.
    /// Used by `CallGraph::build` to fold the edges table into adjacency in
    /// a single scan instead of N * find_callers.
    pub fn iter_call_edges(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT src_symbol, dst_name FROM edges
             WHERE kind = 'call' AND src_symbol IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list_files(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT path, lang FROM files")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list_files_with_meta(&self) -> Result<ahash::AHashMap<String, (String, i64)>> {
        // ahash is faster than std HashMap on hot refresh paths (one
        // entry per indexed file; called once per `refresh()`). DoS
        // resistance unnecessary — keys are repo-relative paths from
        // our own walker, never untrusted input. The public surface
        // change is binary-compatible for callers that just `.get()`
        // the map; we type-alias-swap rather than wrap.
        let mut stmt = self.conn.prepare("SELECT path, sha256, mtime FROM files")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut map: ahash::AHashMap<String, (String, i64)> = ahash::AHashMap::new();
        for r in rows {
            let (p, s, m) = r?;
            map.insert(p, (s, m));
        }
        Ok(map)
    }

    pub fn touch_mtime(&self, path: &str, mtime: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET mtime = ?1, indexed_at = strftime('%s','now') WHERE path = ?2",
            params![mtime, path],
        )?;
        Ok(())
    }

    pub fn delete_file(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM files WHERE path = ?1", params![path])?;
        Ok(())
    }

    pub fn clear_all(&self) -> Result<()> {
        self.conn.execute("DELETE FROM files", [])?;
        Ok(())
    }

    /// Read a value from the `meta` table, or `None` if the key isn't set.
    /// Used for boolean flags like `edges_populated` that gate query paths.
    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let v = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .ok();
        Ok(v)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Decode a row's `signature` column, honoring `signature_enc`. Centralized
    /// so all three read paths share identical semantics — the alternative was
    /// copy-pasting the same branch into every `query_map` callback.
    fn signature_from_row(
        &self,
        row: &rusqlite::Row,
        sig_idx: usize,
        enc_idx: usize,
    ) -> rusqlite::Result<Option<String>> {
        // `signature_enc` is non-null with default 0; older databases that
        // somehow lack the column are migrated at open time. Treat read errors
        // as "not encoded" rather than failing the row.
        let enc: i64 = row.get::<_, i64>(enc_idx).unwrap_or(0);

        #[cfg(feature = "compress")]
        {
            if enc == 1 {
                if let Some(codec) = self.codec.as_ref() {
                    // Encoded: read raw bytes, decompress, parse as UTF-8.
                    let bytes: Option<Vec<u8>> = row.get(sig_idx)?;
                    return Ok(match bytes {
                        None => None,
                        Some(b) if b.is_empty() => Some(String::new()),
                        Some(b) => {
                            let plain = codec.decompress(&b);
                            Some(String::from_utf8(plain).map_err(|e| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    sig_idx,
                                    rusqlite::types::Type::Text,
                                    Box::new(e),
                                )
                            })?)
                        }
                    });
                }
                // enc=1 but no codec: row is opaque. Return None to avoid
                // surfacing garbage; this path means the symbols file was
                // deleted out from under us.
                return Ok(None);
            }
        }
        // Plain path (covers feature-disabled builds and enc=0 rows).
        let _ = enc; // silence unused-var warning when feature is off
        row.get::<_, Option<String>>(sig_idx)
    }

    pub fn iter_all_symbols(&self) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.name, s.kind, s.signature, s.parent, f.path, s.line_start, s.line_end, s.visibility, s.signature_enc
             FROM symbols s JOIN files f ON s.file_id = f.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Symbol {
                name: row.get(0)?,
                kind: kind_from_str(&row.get::<_, String>(1)?),
                signature: self.signature_from_row(row, 2, 8)?,
                parent: row.get(3)?,
                file: row.get(4)?,
                line_start: row.get(5)?,
                line_end: row.get(6)?,
                visibility: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn symbols_in_file(&self, file: &str) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.name, s.kind, s.signature, s.parent, f.path, s.line_start, s.line_end, s.visibility, s.signature_enc
             FROM symbols s JOIN files f ON s.file_id = f.id
             WHERE f.path = ?1
             ORDER BY s.line_start",
        )?;
        let rows = stmt.query_map(params![file], |row| {
            Ok(Symbol {
                name: row.get(0)?,
                kind: kind_from_str(&row.get::<_, String>(1)?),
                signature: self.signature_from_row(row, 2, 8)?,
                parent: row.get(3)?,
                file: row.get(4)?,
                line_start: row.get(5)?,
                line_end: row.get(6)?,
                visibility: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn find_by_name(&self, name: &str) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.name, s.kind, s.signature, s.parent, f.path, s.line_start, s.line_end, s.visibility, s.signature_enc
             FROM symbols s JOIN files f ON s.file_id = f.id
             WHERE s.name = ?1",
        )?;
        let rows = stmt.query_map(params![name], |row| {
            Ok(Symbol {
                name: row.get(0)?,
                kind: kind_from_str(&row.get::<_, String>(1)?),
                signature: self.signature_from_row(row, 2, 8)?,
                parent: row.get(3)?,
                file: row.get(4)?,
                line_start: row.get(5)?,
                line_end: row.get(6)?,
                visibility: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// One row of `Store::callers_of` — caller-side hit shape, file-and-line plus
/// the enclosing function name when known. Kept narrow so the SQL layer can
/// stream rows without paying for snippet rendering (the query layer adds
/// snippets on demand from disk).
#[derive(Debug, Clone)]
pub struct EdgeHit {
    pub file: String,
    pub line: u32,
    pub src_symbol: Option<String>,
}

/// In v1.0.0 the `edges.src_symbol` column was INTEGER (FK to symbols.id) but
/// was never populated. v2.0 uses TEXT (the enclosing symbol name) to mirror
/// `dst_name` and avoid a join on every caller query. The table is always
/// empty for v1.0.0 users, so dropping and recreating is loss-free.
fn migrate_edges_text(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(edges)")?;
    let mut rows = stmt.query([])?;
    let mut needs_migrate = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        let coltype: String = row.get(2)?;
        if name == "src_symbol" && coltype.eq_ignore_ascii_case("INTEGER") {
            needs_migrate = true;
            break;
        }
    }
    drop(rows);
    drop(stmt);
    if needs_migrate {
        conn.execute_batch(
            "DROP TABLE IF EXISTS edges;
             CREATE TABLE edges (
                id          INTEGER PRIMARY KEY,
                src_file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                src_symbol  TEXT,
                dst_name    TEXT    NOT NULL,
                kind        TEXT    NOT NULL,
                line        INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_edges_dst      ON edges(dst_name);
             CREATE INDEX IF NOT EXISTS idx_edges_src      ON edges(src_file_id);
             CREATE INDEX IF NOT EXISTS idx_edges_dst_kind ON edges(dst_name, kind);
             INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '2');",
        )?;
    } else {
        // Fresh DB or already migrated — make sure the kind-composite index
        // exists for older v2 builds that predate it.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_edges_dst_kind ON edges(dst_name, kind);
             INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '2');",
        )?;
    }
    Ok(())
}

fn kind_str(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::Function => "function",
        SymbolKind::Method => "method",
        SymbolKind::Class => "class",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Interface => "interface",
        SymbolKind::Const => "const",
        SymbolKind::Var => "var",
        SymbolKind::Type => "type",
        SymbolKind::Macro => "macro",
    }
}

fn kind_from_str(s: &str) -> SymbolKind {
    match s {
        "method" => SymbolKind::Method,
        "class" => SymbolKind::Class,
        "struct" => SymbolKind::Struct,
        "enum" => SymbolKind::Enum,
        "trait" => SymbolKind::Trait,
        "interface" => SymbolKind::Interface,
        "const" => SymbolKind::Const,
        "var" => SymbolKind::Var,
        "type" => SymbolKind::Type,
        "macro" => SymbolKind::Macro,
        _ => SymbolKind::Function,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SymbolKind;

    fn tmp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("idx.db");
        let store = Store::open(&db).unwrap();
        (dir, store)
    }

    fn sym(name: &str, kind: SymbolKind, parent: Option<&str>) -> Symbol {
        Symbol {
            name: name.into(),
            kind,
            signature: Some(format!("fn {name}(...)")),
            parent: parent.map(String::from),
            file: "a.ts".into(),
            line_start: 1,
            line_end: 5,
            visibility: None,
        }
    }

    #[test]
    fn upsert_then_find() {
        let (_dir, store) = tmp_store();
        let fid = store
            .upsert_file("a.ts", "deadbeef", 0, "typescript")
            .unwrap();
        store
            .replace_symbols(fid, &[sym("foo", SymbolKind::Function, None)])
            .unwrap();

        let hits = store.find_by_name("foo").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file, "a.ts");
        assert!(matches!(hits[0].kind, SymbolKind::Function));
    }

    #[test]
    fn replace_symbols_overwrites() {
        let (_dir, store) = tmp_store();
        let fid = store.upsert_file("a.ts", "h1", 0, "typescript").unwrap();
        store
            .replace_symbols(fid, &[sym("foo", SymbolKind::Function, None)])
            .unwrap();
        store
            .replace_symbols(fid, &[sym("bar", SymbolKind::Function, None)])
            .unwrap();

        assert_eq!(store.find_by_name("foo").unwrap().len(), 0);
        assert_eq!(store.find_by_name("bar").unwrap().len(), 1);
    }

    #[test]
    fn upsert_file_returns_stable_id() {
        let (_dir, store) = tmp_store();
        let a = store.upsert_file("a.ts", "h1", 0, "typescript").unwrap();
        let b = store.upsert_file("a.ts", "h2", 1, "typescript").unwrap();
        assert_eq!(a, b, "upsert on same path must return the same row id");
        store
            .replace_symbols(b, &[sym("x", SymbolKind::Function, None)])
            .unwrap();
        assert_eq!(store.find_by_name("x").unwrap().len(), 1);
    }

    #[test]
    fn find_by_name_returns_method_with_parent() {
        let (_dir, store) = tmp_store();
        let fid = store.upsert_file("a.ts", "h", 0, "typescript").unwrap();
        store
            .replace_symbols(fid, &[sym("greet", SymbolKind::Method, Some("Greeter"))])
            .unwrap();
        let hits = store.find_by_name("greet").unwrap();
        assert_eq!(hits[0].parent.as_deref(), Some("Greeter"));
    }

    #[test]
    fn list_files_returns_all_indexed() {
        let (_dir, store) = tmp_store();
        store.upsert_file("a.ts", "h", 0, "typescript").unwrap();
        store.upsert_file("b.rb", "h", 0, "ruby").unwrap();
        let files = store.list_files().unwrap();
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"a.ts"));
        assert!(paths.contains(&"b.rb"));
        let langs: std::collections::HashSet<&str> =
            files.iter().map(|(_, l)| l.as_str()).collect();
        assert!(langs.contains("typescript"));
        assert!(langs.contains("ruby"));
    }

    #[test]
    fn delete_file_cascades_to_symbols() {
        let (_dir, store) = tmp_store();
        let fid = store.upsert_file("a.ts", "h", 0, "typescript").unwrap();
        store
            .replace_symbols(fid, &[sym("foo", SymbolKind::Function, None)])
            .unwrap();
        assert_eq!(store.find_by_name("foo").unwrap().len(), 1);

        store.delete_file("a.ts").unwrap();
        // ON DELETE CASCADE on the schema should drop the symbols too.
        assert_eq!(
            store.find_by_name("foo").unwrap().len(),
            0,
            "delete_file did not cascade to symbols"
        );
    }

    #[test]
    fn list_files_with_meta_round_trips_sha_mtime() {
        let (_dir, store) = tmp_store();
        store
            .upsert_file("a.ts", "deadbeef", 1234, "typescript")
            .unwrap();
        store.upsert_file("b.rb", "feedface", 5678, "ruby").unwrap();
        let map = store.list_files_with_meta().unwrap();
        assert_eq!(map.get("a.ts"), Some(&("deadbeef".into(), 1234)));
        assert_eq!(map.get("b.rb"), Some(&("feedface".into(), 5678)));
    }

    #[test]
    fn touch_mtime_updates_only_mtime() {
        let (_dir, store) = tmp_store();
        store
            .upsert_file("a.ts", "sha1", 100, "typescript")
            .unwrap();
        store.touch_mtime("a.ts", 200).unwrap();
        let map = store.list_files_with_meta().unwrap();
        let (sha, mt) = map.get("a.ts").unwrap();
        assert_eq!(sha, "sha1", "sha must not change on touch");
        assert_eq!(*mt, 200);
    }

    #[test]
    fn iter_all_symbols_sees_every_file() {
        let (_dir, store) = tmp_store();
        let f1 = store.upsert_file("a.ts", "h", 0, "typescript").unwrap();
        let f2 = store.upsert_file("b.rb", "h", 0, "ruby").unwrap();
        store
            .replace_symbols(f1, &[sym("foo", SymbolKind::Function, None)])
            .unwrap();
        store
            .replace_symbols(f2, &[sym("Bar", SymbolKind::Class, None)])
            .unwrap();
        let all = store.iter_all_symbols().unwrap();
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"Bar"));
    }

    #[test]
    fn store_is_send() {
        // Compile-time check that we can move a Store across threads.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("idx.db")).unwrap();
        std::thread::spawn(move || {
            // Just touch the moved store so the closure isn't optimized away.
            let _ = store.list_files();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn macro_kind_round_trips_through_sqlite() {
        // The Macro variant is new in v1.1 — it must survive the
        // SymbolKind -> "macro" -> SymbolKind path through the store.
        let (_dir, store) = tmp_store();
        let fid = store.upsert_file("log.rs", "h", 0, "rust").unwrap();
        store
            .replace_symbols(fid, &[sym("info", SymbolKind::Macro, None)])
            .unwrap();
        let hits = store.find_by_name("info").unwrap();
        assert_eq!(hits.len(), 1);
        assert!(matches!(hits[0].kind, SymbolKind::Macro), "{:?}", hits[0]);
    }

    #[test]
    fn all_symbol_kinds_round_trip_through_sqlite() {
        // Belt-and-braces: every variant of SymbolKind must survive
        // serialize-via-store-roundtrip. Catches missing arms in
        // `kind_str` / `kind_from_str` if anyone adds a new variant.
        let (_dir, store) = tmp_store();
        let kinds = [
            ("f1", SymbolKind::Function),
            ("m1", SymbolKind::Method),
            ("c1", SymbolKind::Class),
            ("s1", SymbolKind::Struct),
            ("e1", SymbolKind::Enum),
            ("t1", SymbolKind::Trait),
            ("i1", SymbolKind::Interface),
            ("k1", SymbolKind::Const),
            ("v1", SymbolKind::Var),
            ("y1", SymbolKind::Type),
            ("r1", SymbolKind::Macro),
        ];
        let fid = store.upsert_file("a.rs", "h", 0, "rust").unwrap();
        let symbols: Vec<Symbol> = kinds.iter().map(|(n, k)| sym(n, *k, None)).collect();
        store.replace_symbols(fid, &symbols).unwrap();
        let listed = store.iter_all_symbols().unwrap();
        for (name, expect_kind) in kinds {
            let s = listed
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(s.kind, expect_kind, "kind mismatch for {name}: {s:?}");
        }
    }

    #[test]
    fn upsert_file_replaces_old_symbols_on_change() {
        // Re-indexing (refresh) calls upsert_file with the same path but a
        // new sha256. The old symbols for that path must be cleared before
        // new ones land — otherwise `find_by_name` returns stale hits.
        let (_dir, store) = tmp_store();
        let fid = store.upsert_file("a.rs", "h1", 1, "rust").unwrap();
        store
            .replace_symbols(fid, &[sym("old_name", SymbolKind::Function, None)])
            .unwrap();
        // Re-upsert with new content + new symbol set.
        let fid2 = store.upsert_file("a.rs", "h2", 2, "rust").unwrap();
        store
            .replace_symbols(fid2, &[sym("new_name", SymbolKind::Function, None)])
            .unwrap();
        assert!(store.find_by_name("old_name").unwrap().is_empty());
        assert_eq!(store.find_by_name("new_name").unwrap().len(), 1);
    }

    #[test]
    fn find_by_name_returns_empty_for_unknown_name() {
        // Defensive: empty result, not an error, when the name doesn't exist.
        let (_dir, store) = tmp_store();
        let _fid = store.upsert_file("a.rs", "h", 0, "rust").unwrap();
        let hits = store.find_by_name("does_not_exist").unwrap();
        assert!(hits.is_empty());
    }
}
