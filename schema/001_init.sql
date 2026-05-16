-- crabcc index schema (v1)
-- Mirrors codeindex.cc shape: files keyed by sha256, symbols + edges normalized.

PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS files (
    id        INTEGER PRIMARY KEY,
    path      TEXT    NOT NULL UNIQUE,
    sha256    TEXT    NOT NULL,
    mtime     INTEGER NOT NULL,
    lang      TEXT    NOT NULL,
    indexed_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS symbols (
    id         INTEGER PRIMARY KEY,
    file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name       TEXT    NOT NULL,
    kind       TEXT    NOT NULL,           -- function|method|class|struct|enum|trait|const|var|type
    signature  TEXT,                        -- compact one-line signature
    parent     TEXT,                        -- enclosing symbol name (nullable)
    line_start INTEGER NOT NULL,
    line_end   INTEGER NOT NULL,
    visibility TEXT,                        -- pub|priv|pkg (lang-dependent)
    -- 0 = plain UTF-8, 1 = FSST-encoded (see crates/crabcc-core/src/compress.rs)
    signature_enc INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_symbols_name        ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_file        ON symbols(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_kind        ON symbols(kind);
-- Outline queries (`crabcc outline FILE`) join symbols→files on file_id and
-- ORDER BY line_start. Covering this with a composite index avoids a sort.
CREATE INDEX IF NOT EXISTS idx_symbols_file_line   ON symbols(file_id, line_start);
-- Symbol-name + kind filter (used when `sym` may grow filter flags).
CREATE INDEX IF NOT EXISTS idx_symbols_name_kind   ON symbols(name, kind);
-- File listings filter by lang. Small table (~13k rows on mc-mothership)
-- but the index makes `crabcc files --lang ruby` a constant-time SQL.
CREATE INDEX IF NOT EXISTS idx_files_lang          ON files(lang);

-- references / call edges. src_symbol = enclosing symbol *name* (null for
-- top-level usage). Name-based to mirror dst_name and the graph adjacency
-- structure (BTreeMap<String, BTreeSet<String>>) — avoids a join on every
-- caller query. v1.0.0 stored this column as INTEGER (FK to symbols.id) but
-- it was never populated; store.rs migrates old DBs in-place on open.
CREATE TABLE IF NOT EXISTS edges (
    id          INTEGER PRIMARY KEY,
    src_file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    src_symbol  TEXT,                       -- enclosing symbol name; null = file-level
    dst_name    TEXT    NOT NULL,           -- target symbol name (unresolved)
    kind        TEXT    NOT NULL,           -- call|import|inherit|impl|ref
    line        INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_edges_dst        ON edges(dst_name);
CREATE INDEX IF NOT EXISTS idx_edges_src        ON edges(src_file_id);
-- Caller lookups filter by (dst_name, kind='call'); composite covers it.
CREATE INDEX IF NOT EXISTS idx_edges_dst_kind   ON edges(dst_name, kind);

-- meta (schema version, last full reindex, etc.)
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '3');
