# Task 1 — Schema v4: symbol-ID-keyed edges + unresolved_names sentinel table

## Context

v4.0 replaces the current name-based `edges` table with symbol-ID FKs. The
`symbols` table grows a `qualified` column (e.g. "crate::module::Foo") and a
`parent_id` self-reference (replacing the loose `parent TEXT`). Pre-v4 indexes
are auto-wiped on first open via the existing `needs_reindex` plumbing.

## What to change

File: `schema/001_init.sql`

Replace the entire file contents with:

```sql
-- crabcc index schema (v4) — symbol-ID-keyed edges, unresolved sentinel.
-- Mirrors codeindex.cc shape: files keyed by sha256; symbols + edges normalized.

PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS files (
    id         INTEGER PRIMARY KEY,
    path       TEXT    NOT NULL UNIQUE,
    sha256     TEXT    NOT NULL,
    mtime      INTEGER NOT NULL,
    lang       TEXT    NOT NULL,
    indexed_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS symbols (
    id            INTEGER PRIMARY KEY,
    file_id       INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    qualified     TEXT,                          -- "crate::module::Foo" when extractable
    kind          TEXT    NOT NULL,              -- function|method|class|struct|enum|trait|const|var|type|sentinel
    parent_id     INTEGER REFERENCES symbols(id),
    line_start    INTEGER NOT NULL,
    line_end      INTEGER NOT NULL,
    signature     TEXT,
    -- 0 = plain UTF-8, 1 = FSST-encoded
    signature_enc INTEGER NOT NULL DEFAULT 0,
    visibility    TEXT
);

CREATE INDEX IF NOT EXISTS idx_symbols_name      ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_file      ON symbols(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_kind      ON symbols(kind);
CREATE INDEX IF NOT EXISTS idx_symbols_qual      ON symbols(qualified);
CREATE INDEX IF NOT EXISTS idx_symbols_file_line ON symbols(file_id, line_start);
CREATE INDEX IF NOT EXISTS idx_symbols_name_kind ON symbols(name, kind);
CREATE INDEX IF NOT EXISTS idx_files_lang        ON files(lang);

-- v4: edges are symbol-ID FKs. dst_symbol_id may point at a sentinel row in
-- symbols where kind='sentinel' (for unresolved names, see unresolved_names).
CREATE TABLE IF NOT EXISTS edges (
    id              INTEGER PRIMARY KEY,
    src_symbol_id   INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    dst_symbol_id   INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    kind            TEXT    NOT NULL CHECK (kind IN ('call','ref','import','inherit','impl')),
    line            INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_edges_dst       ON edges(dst_symbol_id);
CREATE INDEX IF NOT EXISTS idx_edges_src       ON edges(src_symbol_id);
CREATE INDEX IF NOT EXISTS idx_edges_dst_kind  ON edges(dst_symbol_id, kind);

-- Sentinel pattern for unresolved edges (languages without a resolver yet,
-- or genuinely ambiguous calls). One sentinel symbol row per unique name;
-- queries that want recall can union edges through these.
CREATE TABLE IF NOT EXISTS unresolved_names (
    symbol_id INTEGER PRIMARY KEY REFERENCES symbols(id) ON DELETE CASCADE,
    name      TEXT    NOT NULL UNIQUE
);
CREATE INDEX IF NOT EXISTS idx_unresolved_name ON unresolved_names(name);

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '4');
```

This is a complete rewrite of `schema/001_init.sql`. The pre-v4 `edges` shape
(`src_file_id, src_symbol TEXT, dst_name TEXT, kind, line`) and pre-v4
`symbols` shape (no `qualified`, `parent TEXT` instead of `parent_id`) are
**dropped**. Pre-v4 indexes will be wiped + rebuilt by the auto-wipe code path
in `Store::open` / `main.rs` — covered by Task 2.

Do not run `cargo build`, `cargo test`, or any other build or test command.

Do not modify any other file. Do not invent extra files.

Then commit with this exact message:

    feat(schema)!: v4 — symbol-ID-keyed edges, qualified col, unresolved_names
