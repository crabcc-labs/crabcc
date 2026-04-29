use crate::types::{Symbol, SymbolKind};
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
}

// Connection is `Send` since rusqlite 0.20; assert at compile time so a future
// dep change can't silently break our threading story.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Store>();
};

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
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
        // Default page cache is 2 MB; bump to ~16 MB. Negative = KiB.
        conn.pragma_update(None, "cache_size", -16_000_i64).ok();
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
        // PRAGMA optimize is a no-op until the query planner has stats; it
        // becomes useful after ANALYZE. Run it whenever we open — sqlite
        // makes the call cheap when nothing's changed.
        let _ = conn.execute_batch("PRAGMA optimize;");
        Ok(Self { conn })
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
        let mut stmt = self.conn.prepare(
            "INSERT INTO symbols(file_id, name, kind, signature, parent, line_start, line_end, visibility)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for s in symbols {
            stmt.execute(params![
                file_id,
                s.name,
                kind_str(s.kind),
                s.signature,
                s.parent,
                s.line_start,
                s.line_end,
                s.visibility
            ])?;
        }
        Ok(())
    }

    pub fn list_files(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT path, lang FROM files")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list_files_with_meta(&self) -> Result<std::collections::HashMap<String, (String, i64)>> {
        let mut stmt = self.conn.prepare("SELECT path, sha256, mtime FROM files")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut map = std::collections::HashMap::new();
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

    pub fn iter_all_symbols(&self) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.name, s.kind, s.signature, s.parent, f.path, s.line_start, s.line_end, s.visibility
             FROM symbols s JOIN files f ON s.file_id = f.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Symbol {
                name: row.get(0)?,
                kind: kind_from_str(&row.get::<_, String>(1)?),
                signature: row.get(2)?,
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
            "SELECT s.name, s.kind, s.signature, s.parent, f.path, s.line_start, s.line_end, s.visibility
             FROM symbols s JOIN files f ON s.file_id = f.id
             WHERE f.path = ?1
             ORDER BY s.line_start",
        )?;
        let rows = stmt.query_map(params![file], |row| {
            Ok(Symbol {
                name: row.get(0)?,
                kind: kind_from_str(&row.get::<_, String>(1)?),
                signature: row.get(2)?,
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
            "SELECT s.name, s.kind, s.signature, s.parent, f.path, s.line_start, s.line_end, s.visibility
             FROM symbols s JOIN files f ON s.file_id = f.id
             WHERE s.name = ?1",
        )?;
        let rows = stmt.query_map(params![name], |row| {
            Ok(Symbol {
                name: row.get(0)?,
                kind: kind_from_str(&row.get::<_, String>(1)?),
                signature: row.get(2)?,
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
}
