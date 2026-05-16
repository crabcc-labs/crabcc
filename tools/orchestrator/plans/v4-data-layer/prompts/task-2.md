# Task 2 — Store API: v4 inserters, sentinel helpers, schema_v4_built flag

## Context

v4.0 keys `edges` by `symbol_id` instead of `dst_name TEXT`. The extractor will
soon do a two-pass walk (definitions first, then uses), so the `Store` needs
three new low-level inserters:

1. `insert_symbol(...)` — single-row insert that returns the new rowid, so
   pass-1 can build a `HashMap<String, SymbolId>` from the rows it just wrote.
2. `insert_edge_resolved(...)` — single-row insert against the new
   symbol-ID-keyed `edges` shape.
3. `upsert_unresolved_sentinel(name)` — get-or-create a sentinel `symbols` row
   so pass-2 can still emit an edge when the resolver returns `None`. This is
   how Ruby/Java/Swift (no resolver yet) and genuinely ambiguous calls survive
   without losing recall.

The auto-wipe gating key moves from `ref_edges_built` (v3.2) to
`schema_v4_built` (v4.0). The two existing tests that exercise the v3.2 flag
must be updated.

## What to change

File: `crates/crabcc-core/src/store.rs`

### Change 1 — Add a sentinel-file constant near the top of the file

After the `const SCHEMA: &str = include_str!(...);` line (around line 6), add:

```rust
/// Sentinel `files` row path used to anchor unresolved-name `symbols` rows.
/// The v4 `symbols.file_id` column is `NOT NULL REFERENCES files(id)`, so
/// sentinel symbols need a real (synthetic) file to live under. Created
/// lazily by `upsert_unresolved_sentinel`. One row per index.
const UNRESOLVED_FILE_PATH: &str = "<unresolved>";
const UNRESOLVED_FILE_LANG: &str = "_unresolved";
```

### Change 2 — Update the `needs_reindex` computation in `open_with_compress`

Find this exact block (around lines 127–147):

```rust
        // `ref_edges_built` is written by Store::mark_ref_edges_built() after a
        // successful full_index. An index that was populated before v3.2.0 lacks
        // this key and needs a wipe + rebuild to gain ref edges. Fresh empty
        // stores are excluded: nothing to rebuild, and auto_index will handle
        // the initial indexing through its own path.
        let has_files = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM files LIMIT 1)", [], |r| {
                r.get::<_, bool>(0)
            })
            .unwrap_or(false);
        let ref_edges_built = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'ref_edges_built'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .unwrap_or(None)
            .as_deref()
            == Some("1");
        let needs_reindex = has_files && !ref_edges_built;
```

Replace it with:

```rust
        // `schema_v4_built` is written by Store::mark_schema_v4_built() after a
        // successful full_index under the v4 schema. Any pre-v4 index (which
        // wrote either nothing or the v3.2 `ref_edges_built` flag instead)
        // lacks this key and must be wiped + rebuilt so its `edges` rows use
        // symbol-ID FKs instead of the dropped `dst_name TEXT` shape. Fresh
        // empty stores are excluded: nothing to rebuild, and auto_index will
        // handle the initial indexing through its own path.
        let has_files = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM files LIMIT 1)", [], |r| {
                r.get::<_, bool>(0)
            })
            .unwrap_or(false);
        let schema_v4_built = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_v4_built'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .unwrap_or(None)
            .as_deref()
            == Some("1");
        let needs_reindex = has_files && !schema_v4_built;
```

### Change 3 — Rename `mark_ref_edges_built` → `mark_schema_v4_built`

Find this exact block (around lines 398–404):

```rust
    /// Record that this index was built with ref-edge extraction support.
    /// Call after every successful `full_index` so subsequent opens — including
    /// those from MCP and LSP that do not act on `needs_reindex` — see the
    /// correct flag and do not clear work done by an earlier rebuild.
    pub fn mark_ref_edges_built(&self) -> Result<()> {
        self.meta_set("ref_edges_built", "1")
    }
```

Replace it with:

```rust
    /// Record that this index was built under the v4 schema (symbol-ID-keyed
    /// edges). Call after every successful `full_index` so subsequent opens —
    /// including those from MCP and LSP that do not act on `needs_reindex` —
    /// see the correct flag and do not clear work done by an earlier rebuild.
    pub fn mark_schema_v4_built(&self) -> Result<()> {
        self.meta_set("schema_v4_built", "1")
    }
```

### Change 4 — Add the three new v4 inserters

Insert these three methods INSIDE the `impl Store { ... }` block, immediately
AFTER the `pub fn mark_schema_v4_built` method you just renamed (i.e. between
`mark_schema_v4_built` and `fn signature_from_row`). The exact insertion point
is right after the closing `}` of `mark_schema_v4_built` and before the
`/// Decode a row's signature column ...` doc comment.

```rust
    /// Insert one symbol row and return its `rowid`. Used by the two-pass
    /// extractor (pass 1) to build a local-defs map keyed by the rowid the
    /// resolver will later return. `qualified` is the fully-qualified name
    /// (e.g. `"crate::module::Foo"`) when the extractor can compute it;
    /// `None` is fine. `parent_id` is the enclosing `impl`/`class`/`module`
    /// symbol's rowid, or `None` for top-level defs.
    ///
    /// `signature_enc` is always written as 0 here — FSST encoding is owned
    /// by the bulk `replace_symbols` path, which has the codec in scope.
    /// Per-row inserts stay plain-text; encoding can be added later without
    /// breaking the API.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_symbol(
        &self,
        file_id: i64,
        name: &str,
        qualified: Option<&str>,
        kind: SymbolKind,
        parent_id: Option<i64>,
        line_start: i64,
        line_end: i64,
        signature: Option<&str>,
        visibility: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO symbols(file_id, name, qualified, kind, parent_id,
                                  line_start, line_end, signature, signature_enc, visibility)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9)",
            params![
                file_id,
                name,
                qualified,
                kind_str(kind),
                parent_id,
                line_start,
                line_end,
                signature,
                visibility,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Insert one resolved edge. `kind` must be one of `'call' | 'ref' |
    /// 'import' | 'inherit' | 'impl'` (the schema CHECK constraint enforces
    /// this; we surface a SQL error if a caller passes anything else).
    pub fn insert_edge_resolved(
        &self,
        src_symbol_id: i64,
        dst_symbol_id: i64,
        kind: &str,
        line: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO edges(src_symbol_id, dst_symbol_id, kind, line)
             VALUES (?1, ?2, ?3, ?4)",
            params![src_symbol_id, dst_symbol_id, kind, line],
        )?;
        Ok(())
    }

    /// Get-or-create a sentinel `symbols` row for an unresolved name. Returns
    /// the sentinel symbol's id, suitable as `dst_symbol_id` in
    /// `insert_edge_resolved`. Idempotent — the same `name` always maps to
    /// the same rowid.
    ///
    /// Design note: schema v4 makes `symbols.file_id` `NOT NULL`, so sentinel
    /// symbols can't be file-less. We park them all under a single synthetic
    /// `files` row (path=`<unresolved>`, lang=`_unresolved`, sha256=`0`,
    /// mtime=0), created lazily on first call. The `unresolved_names` table
    /// holds the `(name, symbol_id)` mapping; `UNIQUE(name)` makes the
    /// SELECT-then-INSERT race-safe under the single-writer model.
    pub fn upsert_unresolved_sentinel(&self, name: &str) -> Result<i64> {
        // Fast path: already mapped.
        if let Some(id) = self
            .conn
            .query_row(
                "SELECT symbol_id FROM unresolved_names WHERE name = ?1",
                params![name],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok(id);
        }

        // Ensure the synthetic anchor file exists. `upsert_file` is idempotent
        // and gives us the rowid back regardless of whether it was new or
        // already present, so it's safe to call on every miss.
        let anchor_file_id =
            self.upsert_file(UNRESOLVED_FILE_PATH, "0", 0, UNRESOLVED_FILE_LANG)?;

        // Create the sentinel symbol. kind='sentinel' so callers can filter it
        // out of normal symbol queries. line_start/end = 0 (no source).
        self.conn.execute(
            "INSERT INTO symbols(file_id, name, qualified, kind, parent_id,
                                  line_start, line_end, signature, signature_enc, visibility)
             VALUES (?1, ?2, NULL, 'sentinel', NULL, 0, 0, NULL, 0, NULL)",
            params![anchor_file_id, name],
        )?;
        let sym_id = self.conn.last_insert_rowid();

        // Record the mapping. `INSERT OR IGNORE` defends against a hypothetical
        // race in case anyone ever wraps this in concurrent writers.
        self.conn.execute(
            "INSERT OR IGNORE INTO unresolved_names(symbol_id, name) VALUES (?1, ?2)",
            params![sym_id, name],
        )?;
        // Re-read so we return the canonical id even if INSERT OR IGNORE no-op'd.
        let final_id: i64 = self.conn.query_row(
            "SELECT symbol_id FROM unresolved_names WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        Ok(final_id)
    }

```

### Change 5 — Update the two tests that referenced `ref_edges_built`

Find this exact test (around lines 637–656):

```rust
    #[test]
    fn populated_store_without_mark_needs_reindex() {
        // Simulates a pre-v3.2.0 index: has indexed files but ref_edges_built
        // was never written. Must trigger needs_reindex on every open so the
        // CLI (and not MCP/LSP) rebuilds it.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("idx.db");

        {
            let store = Store::open(&db).unwrap();
            store.upsert_file("a.rs", "deadbeef", 0, "rust").unwrap();
            // deliberately do NOT call mark_ref_edges_built
        }

        let store = Store::open(&db).unwrap();
        assert!(
            store.needs_reindex,
            "populated index without ref_edges_built must set needs_reindex"
        );
    }
```

Replace it with:

```rust
    #[test]
    fn populated_store_without_mark_needs_reindex() {
        // Simulates a pre-v4 index: has indexed files but schema_v4_built was
        // never written. Must trigger needs_reindex on every open so the CLI
        // (and not MCP/LSP) rebuilds it under the new symbol-ID edge shape.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("idx.db");

        {
            let store = Store::open(&db).unwrap();
            store.upsert_file("a.rs", "deadbeef", 0, "rust").unwrap();
            // deliberately do NOT call mark_schema_v4_built
        }

        let store = Store::open(&db).unwrap();
        assert!(
            store.needs_reindex,
            "populated index without schema_v4_built must set needs_reindex"
        );
    }
```

Find this exact test (around lines 658–671):

```rust
    #[test]
    fn mark_ref_edges_built_clears_needs_reindex() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("idx.db");

        {
            let store = Store::open(&db).unwrap();
            store.upsert_file("a.rs", "deadbeef", 0, "rust").unwrap();
            store.mark_ref_edges_built().unwrap();
        }

        let store = Store::open(&db).unwrap();
        assert!(!store.needs_reindex, "DB must not need reindex after mark");
    }
```

Replace it with:

```rust
    #[test]
    fn mark_schema_v4_built_clears_needs_reindex() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("idx.db");

        {
            let store = Store::open(&db).unwrap();
            store.upsert_file("a.rs", "deadbeef", 0, "rust").unwrap();
            store.mark_schema_v4_built().unwrap();
        }

        let store = Store::open(&db).unwrap();
        assert!(!store.needs_reindex, "DB must not need reindex after mark");
    }
```

## Notes for the implementer

- Do NOT touch `replace_symbols`, `replace_edges`, `callers_of`, `refs_of`,
  `iter_call_edges`, or any other existing query method in this task.
  Those operate against the v3 column shape and will be reworked by later
  tasks once the extractor populates the v4 columns. Leaving them temporarily
  out-of-sync with the schema is intentional — Task 1 already moved the
  schema; this task only adds the v4 writer surface and gates auto-wipe.
- The schema declared in Task 1 has `parent_id INTEGER REFERENCES symbols(id)`
  and a `qualified TEXT` column on `symbols`. The new `insert_symbol` writes
  those directly. `replace_symbols`' old `parent TEXT` write path will fail at
  runtime; that's fine for this isolated commit because it's not exercised by
  the tests touched here.
- `migrate_edges_text` near the bottom of the file is dead under v4 (the new
  schema has no `src_symbol` INTEGER column for it to migrate from). Leave it
  in place — a later task will remove it as part of the extractor cutover.
- Keep `use rusqlite::{params, Connection, OptionalExtension};` — the new
  `upsert_unresolved_sentinel` uses `.optional()` from `OptionalExtension`.

Do not run `cargo build`, `cargo test`, or any other build or test command.

Do not modify any other file. Do not invent extra files.

Then commit with this exact message:

    feat(store)!: v4 API — insert_symbol, insert_edge_resolved, sentinel helpers
